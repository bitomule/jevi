//! jevi — ask typed questions about a text and branch on the answer.
//!
//! This is the shell around the library: argument parsing, printing, and the exit-code map.
//! Nothing here decides anything about an answer; that all lives in the `jevi` crate, so a
//! caller that wants the judgements without the terminal can depend on the library alone.
//!
//! The exit-code map has one hard rule: never exit 2. Claude Code reads a hook's exit 2 as
//! "block this tool call and hand my stderr to the model", so a mistyped flag in a hook
//! would become a block on the thing it hooks. clap exits 2 by default, which is why this
//! uses `try_parse` and maps its errors to 5.

mod cli;

use std::io::{IsTerminal, Read, Write};

use clap::Parser;
use serde_json::Value;

use jevi::{AskOptions, Config, Error, Provider, QuestionSet, Result, Verdict};

use crate::cli::{Ask, Cli, Command, ConfigCmd};

fn main() {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // `--help` and `--version` arrive here too, and they are a success.
            let ok = matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            let _ = e.print();
            std::process::exit(if ok { 0 } else { 5 });
        }
    };

    let soft = matches!(&cli.command, Command::Ask(a) if a.soft);

    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            if soft {
                println!("{}", jevi::failure_document(e.kind(), &e.to_string()));
                0
            } else {
                eprintln!("jevi: {e}");
                e.code()
            }
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Ask(a) => ask(a),
        Command::Doctor { live } => doctor(live),
        Command::Config(ConfigCmd::SetKey { provider }) => set_key(&provider),
    }
}

fn ask(a: Ask) -> Result<i32> {
    // A caller can turn the whole thing off without editing any of the scripts that call it.
    if std::env::var("JEVI_DISABLE").is_ok_and(|v| !v.is_empty() && v != "0") {
        return Err(Error::no_answer("disabled", "JEVI_DISABLE is set"));
    }

    let cfg = Config::load()?;
    let prepared = match (&a.set, &a.instructions) {
        (Some(_), Some(_)) => {
            return Err(Error::invalid("flags", "give a question or -f, not both"))
        }
        (Some(name), None) => {
            let (raw, origin) = cfg.resolve_set(name)?;
            QuestionSet::parse(&raw, &origin)?.prepare()?
        }
        (None, Some(text)) => jevi::internal::shorthand(
            text,
            a.options.as_deref(),
            a.levels.as_deref(),
            a.yes_at,
            a.no_at,
            a.min_confidence,
        )?,
        (None, None) => {
            return Err(Error::invalid(
                "flags",
                "no question given — try `jevi ask --help`",
            ))
        }
    };

    let single = prepared.names.len() == 1 && a.set.is_none();
    let raw_state = read_state(&a)?;

    // The cap is the question set's when it has one, because that is the length its
    // thresholds were measured at.
    let cap = a.max_chars.or(prepared.max_chars).unwrap_or(80_000);
    let (state, state_chars) = build_state(&raw_state, a.state_json, cap)?;

    let opts = AskOptions {
        provider: a.provider.clone(),
        model: a.model.clone(),
        timeout_ms: a.timeout,
        state_chars: Some(state_chars),
    };

    if a.raw {
        println!("{}", jevi::ask_raw(&cfg, &prepared, &state, &opts)?);
        return Ok(0);
    }

    let answered = jevi::ask(&cfg, &prepared, &state, &opts)?;

    if a.json {
        println!("{}", jevi::document(&answered));
    } else {
        print!(
            "{}",
            jevi::internal::terse(&answered.names, &answered.outcomes, single)
        );
    }

    if a.verbose {
        let usage = answered
            .usage
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        eprintln!("jevi: {}ms {usage}", answered.latency_ms);
    }

    // The code describes the first question, except that any uncertainty anywhere wins:
    // a caller gating on this should stop when any part of the answer is unknown.
    let any_unsure = answered
        .outcomes
        .iter()
        .any(|o| o.verdict == Verdict::Unsure);
    Ok(if any_unsure {
        3
    } else {
        answered.outcomes[0].verdict.code()
    })
}

fn read_state(a: &Ask) -> Result<String> {
    if let Some(t) = &a.state_text {
        return Ok(t.clone());
    }
    if let Some(path) = &a.state {
        return std::fs::read_to_string(path)
            .map_err(|e| Error::invalid("state", format!("{path}: {e}")));
    }
    if std::io::stdin().is_terminal() {
        return Err(Error::invalid(
            "state",
            "nothing on stdin — pipe the text in, or use --text/--state",
        ));
    }
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| Error::invalid("state", e.to_string()))?;
    Ok(buf)
}

fn build_state(raw: &str, as_json: bool, cap: usize) -> Result<(Value, usize)> {
    if raw.trim().is_empty() {
        return Err(Error::invalid("state", "the text to judge is empty"));
    }
    if as_json {
        // Structured state is never truncated: cutting an object produces invalid JSON, and
        // a silently reshaped state is worse than a loud 400 from the API.
        let v: Value = serde_json::from_str(raw)
            .map_err(|e| Error::invalid("state", format!("--state-json but not JSON: {e}")))?;
        let len = raw.chars().count();
        return Ok((v, len));
    }
    let len = raw.chars().count();
    if cap > 0 && len > cap {
        let head: String = raw.chars().take(cap).collect();
        let cut = len - cap;
        return Ok((
            Value::String(format!("{head}\n\n[jevi: {cut} characters truncated]")),
            cap,
        ));
    }
    Ok((Value::String(raw.to_owned()), len))
}

fn doctor(live: bool) -> Result<i32> {
    let cfg = Config::load()?;
    let p = cfg.provider(None)?;
    let path = jevi::config_path();

    println!(
        "config:    {} ({})",
        path.display(),
        if cfg.loaded { "found" } else { "absent" }
    );
    if cfg.loaded {
        if let Ok(meta) = std::fs::metadata(&path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode() & 0o777;
            println!(
                "mode:      {mode:04o}{}",
                if mode == 0o600 { "" } else { "  (want 0600)" }
            );
        }
    }
    println!("provider:  {}", p.name());
    println!(
        "key:       {}",
        if cfg.key_for(p).is_some() {
            "present"
        } else {
            "MISSING"
        }
    );
    println!("model:     {}", cfg.model(p, None));
    println!("url:       {}", cfg.url(p));
    println!(
        "questions: {}",
        jevi::config_dir().join("questions").display()
    );
    if std::env::var("JEVI_DISABLE").is_ok_and(|v| !v.is_empty() && v != "0") {
        println!("JEVI_DISABLE is set: every `ask` will return no answer.");
    }

    if !live {
        return Ok(0);
    }

    let prepared = jevi::shorthand("This text mentions a cat.")?;
    let state = Value::String("The cat sat on the mat.".into());
    let answered = jevi::ask(&cfg, &prepared, &state, &AskOptions::default())?;
    println!(
        "live:      ok in {}ms, model {}, usage {}",
        answered.latency_ms,
        answered.model.as_deref().unwrap_or("?"),
        answered
            .usage
            .map(|u| u.to_string())
            .unwrap_or_else(|| "?".into())
    );
    Ok(0)
}

fn set_key(name: &str) -> Result<i32> {
    let p = Provider::parse(name)?;
    let mut key = String::new();
    std::io::stdin()
        .read_to_string(&mut key)
        .map_err(|e| Error::invalid("stdin", e.to_string()))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(Error::invalid("stdin", "no key on stdin"));
    }

    let path = jevi::config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::invalid("config", format!("{}: {e}", dir.display())))?;
    }
    let mut root: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| Value::Object(Default::default()));
    if !root.is_object() {
        root = Value::Object(Default::default());
    }
    root[p.name()]["api_key"] = Value::String(key.to_owned());

    let body =
        serde_json::to_string_pretty(&root).map_err(|e| Error::invalid("config", e.to_string()))?;
    write_private(&path, &body)?;
    println!("stored {} in {}", p.env_key(), path.display());
    Ok(0)
}

fn write_private(path: &std::path::Path, body: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    // Written through a temporary and renamed, so a crash never leaves a half-written
    // config, and created 0600 from the start rather than chmod'ed after the fact.
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| Error::invalid("config", format!("{}: {e}", tmp.display())))?;
    f.write_all(body.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| Error::invalid("config", e.to_string()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::invalid("config", format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_string_state_is_truncated_and_says_so() {
        let raw = "x".repeat(200);
        let (v, chars) = build_state(&raw, false, 50).expect("builds");
        assert_eq!(chars, 50);
        assert!(v
            .as_str()
            .expect("string")
            .contains("150 characters truncated"));
    }

    #[test]
    fn a_json_state_is_never_truncated() {
        let raw = format!(r#"{{"a":"{}"}}"#, "x".repeat(200));
        let (v, _) = build_state(&raw, true, 50).expect("builds");
        assert_eq!(v["a"].as_str().expect("string").len(), 200);
    }

    #[test]
    fn an_empty_state_is_refused_before_any_network() {
        assert_eq!(build_state("   \n", false, 0).expect_err("fails").code(), 5);
    }
}
