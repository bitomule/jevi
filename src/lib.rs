//! Ask typed questions about a text and get typed answers back.
//!
//! This is the library half of `jevi`. The binary is a thin shell over it: argument
//! parsing, the exit-code map and the printing live there, and nothing in here knows that a
//! command line exists. That split is what lets another crate depend on this without
//! dragging in `clap` and the terminal machinery.
//!
//! The surface is deliberately small, because every name exported here is a semver promise.
//! If you need something that is private, ask for it to be exposed on purpose rather than
//! finding it exposed by accident.
//!
//! ```no_run
//! # fn main() -> jevi::Result<()> {
//! use serde_json::json;
//!
//! let cfg = jevi::Config::load()?;
//! let set = jevi::QuestionSet::parse(&std::fs::read_to_string("triage.json").unwrap(), "triage.json")?;
//! let prepared = set.prepare()?;
//! let state = json!({ "report": "the build went red after the rebase" });
//!
//! match jevi::ask(&cfg, &prepared, &state, &jevi::AskOptions::default()) {
//!     Ok(answered) => {
//!         for (name, outcome) in answered.names.iter().zip(&answered.outcomes) {
//!             println!("{name}: {}", outcome.verdict.as_str());
//!         }
//!     }
//!     // Two failure classes, and the difference is the whole point: `NoAnswer` means the
//!     // question was never asked (no key, no network, a timeout) and a caller should carry
//!     // on as it would have without this; `Invalid` means the request itself was wrong and
//!     // will be wrong again, which is a defect to surface, not a reason to skip.
//!     Err(e) if matches!(e, jevi::Error::NoAnswer { .. }) => { /* degrade */ }
//!     Err(e) => return Err(e),
//! }
//! # Ok(()) }
//! ```

mod config;
mod decide;
mod error;
mod http;
mod output;
mod provider;
mod questions;

use std::time::Instant;

use serde_json::Value;

pub use crate::config::{config_dir, config_path, Config};
pub use crate::decide::{Outcome, Verdict};
pub use crate::error::{Error, Result};
pub use crate::provider::Provider;
pub use crate::questions::{Decide, Prepared, QuestionSet, Validated};

/// Everything one call came back with. `outcomes` lines up with `names` by position.
#[derive(Debug)]
pub struct Answered {
    pub names: Vec<String>,
    pub outcomes: Vec<Outcome>,
    /// The model that actually answered — a dated build of whatever was asked for.
    pub model: Option<String>,
    /// The provider's own usage block, passed through untouched.
    pub usage: Option<Value>,
    pub latency_ms: u128,
    /// The provider used, for the record.
    pub provider: Provider,
}

/// Per-call overrides. All of them default to what the config resolves.
#[derive(Debug, Default)]
pub struct AskOptions {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub timeout_ms: Option<u64>,
    /// How long the state was, in characters, for the `validated.max_chars` check. Leave it
    /// at `None` and it is measured from the state itself.
    pub state_chars: Option<usize>,
}

/// Send one state and its questions, and turn the reply into verdicts.
///
/// Returns `Err(Error::NoAnswer)` when the question could not be put (no credential, no
/// network, a timeout, the service refusing), and `Err(Error::Invalid)` when the request was
/// malformed. Callers that degrade should degrade on the first and surface the second.
pub fn ask(
    cfg: &Config,
    prepared: &Prepared,
    state: &Value,
    opts: &AskOptions,
) -> Result<Answered> {
    let p = cfg.provider(opts.provider.as_deref())?;
    let key = cfg.key_for(p).ok_or_else(|| {
        Error::no_answer(
            "no_key",
            format!(
                "no credential for {} (set {} or run `jevi config set-key {}`)",
                p.name(),
                p.env_key(),
                p.name()
            ),
        )
    })?;
    let model = prepared
        .model
        .clone()
        .unwrap_or_else(|| cfg.model(p, opts.model.as_deref()));

    let body = p.body(&model, state, &prepared.wire);
    let started = Instant::now();
    let response_body = http::post(&cfg.url(p), &key, &body, cfg.timeout_ms(opts.timeout_ms))?;
    let latency_ms = started.elapsed().as_millis();

    let response = provider::read(&response_body)?;
    let state_chars = opts.state_chars.unwrap_or_else(|| state_len(state));
    let provenance = decide::Provenance {
        model_used: response.model.as_deref(),
        state_chars,
    };

    let outcomes = prepared
        .names
        .iter()
        .zip(&prepared.decide)
        .map(|(name, d)| {
            let answer = response.answers.get(name).cloned().unwrap_or(Value::Null);
            decide::outcome(&answer, d, &provenance)
        })
        .collect();

    Ok(Answered {
        names: prepared.names.clone(),
        outcomes,
        model: response.model,
        usage: response.usage,
        latency_ms,
        provider: p,
    })
}

/// The raw provider response, for finding out what an unverified endpoint actually returns.
/// Everything else goes through [`ask`].
pub fn ask_raw(
    cfg: &Config,
    prepared: &Prepared,
    state: &Value,
    opts: &AskOptions,
) -> Result<Value> {
    let p = cfg.provider(opts.provider.as_deref())?;
    let key = cfg
        .key_for(p)
        .ok_or_else(|| Error::no_answer("no_key", format!("no credential for {}", p.name())))?;
    let model = prepared
        .model
        .clone()
        .unwrap_or_else(|| cfg.model(p, opts.model.as_deref()));
    let body = p.body(&model, state, &prepared.wire);
    http::post(&cfg.url(p), &key, &body, cfg.timeout_ms(opts.timeout_ms))
}

fn state_len(state: &Value) -> usize {
    match state {
        Value::String(s) => s.chars().count(),
        other => other.to_string().chars().count(),
    }
}

/// Build a one-off question the way the command line does, for callers that want the same
/// shorthand without going through a file.
pub fn shorthand(instructions: &str) -> Result<Prepared> {
    questions::shorthand(instructions, None, None, None, None, None)
}

/// Render answers as the normalised JSON document the CLI prints with `--json`.
pub fn document(answered: &Answered) -> Value {
    output::document(
        &answered.names,
        &answered.outcomes,
        answered.provider.name(),
        answered.model.as_deref(),
        answered.usage.as_ref(),
        answered.latency_ms,
    )
}

/// What `--soft` prints when there is no answer. Exposed so a caller writing the same kind
/// of degradable record does not have to reinvent the shape.
pub fn failure_document(kind: &str, message: &str) -> Value {
    output::failure(kind, message)
}

#[doc(hidden)]
pub mod internal {
    //! Used by the `jevi` binary. Not a stable interface; do not depend on it.
    pub use crate::output::terse;
    pub use crate::questions::shorthand;
}
