//! The question-set file, and the split between what the API sees and what stays here.
//!
//! A question file carries two things that must not be confused: the question itself, which
//! goes to the model verbatim, and the thresholds, which are a record of a measurement made
//! on this machine against real data. `decide` and `notes` never leave this process.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};

pub const DEFAULT_YES: f64 = 0.9;
pub const DEFAULT_NO: f64 = 0.1;
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.9;

/// Advice printed to stderr: things that are legal, answered, and still worth knowing. It
/// is separated from the warnings that report what actually happened to a call — a
/// truncated state, a forced verdict — because those must never be silenceable, and these
/// fire on ordinary correct usage and would otherwise spam anything calling jevi in a loop.
fn note(msg: &str) {
    if !QUIET.load(std::sync::atomic::Ordering::Relaxed)
        && !std::env::var("JEVI_QUIET").is_ok_and(|v| !v.is_empty() && v != "0")
    {
        eprintln!("jevi: {msg}");
    }
}

static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Silence the advisory notes for this process. `--soft` sets it, because soft mode's whole
/// promise is that a hook calling jevi is never disturbed by it — the test that caught this
/// asserts an empty stderr, and advice on stderr is still noise on stderr.
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, std::sync::atomic::Ordering::Relaxed);
}

/// What the API actually refuses, measured against it: 255 options is accepted and 256
/// comes back `Too many choices. Must have at most 255 choices.`
const MAX_CHOICES: usize = 255;
/// What the documentation recommends. Not a limit — a hint about where accuracy starts to
/// suffer, so it is a warning and never a refusal.
const RECOMMENDED_CHOICES: usize = 8;

/// This refused anything over 8 because the docs say "2-8 options", and turning a
/// recommendation into a hard block cost someone a real use case: choosing among the seven
/// tappable rows of an iOS Settings screen plus "scroll" and "done" is nine, and jevi
/// rejected the call before it ever reached an API that would have answered it fine.
///
/// The rule now is the one a wrapper should follow: refuse what the service refuses, warn
/// about what the service merely discourages.
fn check_choice_len(name: &str, len: usize) -> Result<()> {
    if len < 2 {
        return Err(Error::invalid(
            "question_file",
            format!("question `{name}`: choice needs at least 2 options, got {len}"),
        ));
    }
    if len > MAX_CHOICES {
        return Err(Error::invalid(
            "question_file",
            format!("question `{name}`: choice takes at most {MAX_CHOICES} options, got {len}"),
        ));
    }
    if len > RECOMMENDED_CHOICES {
        note(&format!(
            "question `{name}` has {len} options; TypeSafe documents 2-{RECOMMENDED_CHOICES}. \
             It will answer, but validate that it still answers well at this width."
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionSet {
    #[serde(default = "one")]
    pub version: u32,
    /// Pins the model these thresholds were validated against. A floating model id silently
    /// rots every threshold under it, so a mismatch downgrades the verdict rather than
    /// applying numbers that were measured somewhere else.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_chars: Option<usize>,
    pub questions: Map<String, Value>,
}

fn one() -> u32 {
    1
}

/// What the tool decides with, once the question has been sent.
#[derive(Debug, Clone)]
pub struct Decide {
    pub yes: f64,
    pub no: f64,
    pub min_confidence: f64,
    /// `None` means nobody has measured this question: the output says so rather than
    /// letting a default pass for a finding.
    pub validated: Option<Validated>,
    /// True when a cut was supplied rather than inherited — a `--yes-at` on the command
    /// line, or a key in a `decide` block. It is tracked rather than derived by comparing
    /// against the shipped numbers so that a cut set by hand to the same value as the
    /// default still reads as a cut somebody chose.
    pub tuned: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Validated {
    pub model: Option<String>,
    pub max_chars: Option<usize>,
}

impl Default for Decide {
    fn default() -> Self {
        Decide {
            yes: DEFAULT_YES,
            no: DEFAULT_NO,
            min_confidence: DEFAULT_MIN_CONFIDENCE,
            validated: None,
            tuned: false,
        }
    }
}

/// One question, split in two: `wire` is sent untouched, `decide` never leaves.
#[derive(Debug)]
pub struct Prepared {
    pub names: Vec<String>,
    pub wire: Map<String, Value>,
    pub decide: Vec<Decide>,
    pub model: Option<String>,
    pub max_chars: Option<usize>,
}

impl QuestionSet {
    pub fn parse(raw: &str, origin: &str) -> Result<Self> {
        serde_json::from_str(raw)
            .map_err(|e| Error::invalid("question_file", format!("{origin}: {e}")))
    }

    pub fn prepare(self) -> Result<Prepared> {
        if self.version != 1 {
            return Err(Error::invalid(
                "question_file",
                format!(
                    "unknown version {} (this build understands 1)",
                    self.version
                ),
            ));
        }
        if self.questions.is_empty() {
            return Err(Error::invalid("question_file", "no questions in the set"));
        }

        let mut names = Vec::new();
        let mut wire = Map::new();
        let mut decide = Vec::new();

        for (name, value) in self.questions {
            let obj = value.as_object().ok_or_else(|| {
                Error::invalid(
                    "question_file",
                    format!("question `{name}` is not an object"),
                )
            })?;
            let (w, d) = split_question(&name, obj)?;
            names.push(name.clone());
            wire.insert(name, Value::Object(w));
            decide.push(d);
        }

        Ok(Prepared {
            names,
            wire,
            decide,
            model: self.model,
            max_chars: self.max_chars,
        })
    }
}

fn split_question(name: &str, obj: &Map<String, Value>) -> Result<(Map<String, Value>, Decide)> {
    let bad = |m: String| Error::invalid("question_file", format!("question `{name}`: {m}"));

    let kind = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("missing `type`".into()))?;

    if obj.get("instructions").and_then(Value::as_str).is_none() {
        return Err(bad("missing `instructions`".into()));
    }

    // The criteria rules are the API's, checked here so a typo costs nothing instead of a
    // round trip and a 400.
    match kind {
        "noul" => {
            match obj.get("criteria") {
                Some(c) => {
                    let c = c.as_object().ok_or_else(|| {
                        bad("noul `criteria` must be an object with `true` and `false`".into())
                    })?;
                    if !c.contains_key("true") || !c.contains_key("false") {
                        return Err(bad("noul `criteria` needs both `true` and `false`".into()));
                    }
                }
                // Legal, and answered, and the single cheapest thing you can do to this
                // question to make it harder to steer. The API allows it, so jevi allows
                // it; staying silent about it is what it had no business doing.
                None => note(&missing_criteria(name)),
            }
        }
        "choice" => {
            let c = obj
                .get("criteria")
                .and_then(Value::as_object)
                .ok_or_else(|| bad("choice needs `criteria` as an object of options".into()))?;
            check_choice_len(name, c.len())?;
        }
        "score" => {
            let c = obj
                .get("criteria")
                .and_then(Value::as_array)
                .ok_or_else(|| bad("score needs `criteria` as an array of levels".into()))?;
            if !(2..=10).contains(&c.len()) {
                return Err(bad(format!("score needs 2-10 levels, got {}", c.len())));
            }
        }
        other => return Err(bad(format!("unknown type `{other}`"))),
    }

    let decide = parse_decide(name, obj.get("decide"))?;

    // Everything except our two private keys travels to the API untouched, so a field this
    // build has never heard of still reaches the model.
    let wire = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "decide" && k.as_str() != "notes")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    Ok((wire, decide))
}

/// Measured against this API, not asserted: over 60 paired runs of the same question with
/// an instruction planted in the state, the verdict flipped 10 times with no `criteria` and
/// 1 time with them. That is the whole reason this note exists.
fn missing_criteria(name: &str) -> String {
    format!(
        "question `{name}` is a noul with no `criteria`; it will be answered, but a state \
         carrying its own instructions flips the verdict far more often without them. Give \
         `criteria` a `true` and a `false` saying what each verdict means. Set JEVI_QUIET=1 \
         to silence this."
    )
}

fn parse_decide(name: &str, raw: Option<&Value>) -> Result<Decide> {
    let bad = |m: String| Error::invalid("question_file", format!("question `{name}`: {m}"));
    let Some(raw) = raw else {
        return Ok(Decide::default());
    };
    let obj = raw
        .as_object()
        .ok_or_else(|| bad("`decide` is not an object".into()))?;

    let num = |key: &str, fallback: f64| -> Result<f64> {
        match obj.get(key) {
            None => Ok(fallback),
            Some(v) => {
                let n = v
                    .as_f64()
                    .ok_or_else(|| bad(format!("`decide.{key}` is not a number")))?;
                if !(0.0..=1.0).contains(&n) {
                    return Err(bad(format!("`decide.{key}` must be within 0..1, got {n}")));
                }
                Ok(n)
            }
        }
    };

    let tuned = ["yes", "no", "min_confidence"]
        .iter()
        .any(|k| obj.contains_key(*k));

    let yes = num("yes", DEFAULT_YES)?;
    let no = num("no", DEFAULT_NO)?;
    if no > yes {
        return Err(bad(format!(
            "`decide.no` ({no}) is above `decide.yes` ({yes})"
        )));
    }
    let min_confidence = num("min_confidence", DEFAULT_MIN_CONFIDENCE)?;

    let validated = obj.get("validated").map(|v| Validated {
        model: v.get("model").and_then(Value::as_str).map(str::to_owned),
        max_chars: v
            .get("max_chars")
            .and_then(Value::as_u64)
            .map(|n| n as usize),
    });

    Ok(Decide {
        yes,
        no,
        min_confidence,
        validated,
        tuned,
    })
}

/// The one-off question a human types at a terminal. It gets the shipped defaults and the
/// output marks it, so nobody mistakes a default for a measurement.
pub fn shorthand(
    instructions: &str,
    options: Option<&str>,
    levels: Option<&str>,
    yes_at: Option<f64>,
    no_at: Option<f64>,
    min_confidence: Option<f64>,
) -> Result<Prepared> {
    let mut q = Map::new();
    q.insert(
        "instructions".into(),
        Value::String(instructions.to_owned()),
    );

    match (options, levels) {
        (Some(_), Some(_)) => {
            return Err(Error::invalid(
                "flags",
                "--options and --levels are mutually exclusive",
            ))
        }
        (Some(list), None) => {
            let items = split_list(list, "--options")?;
            check_choice_len("answer", items.len())?;
            let map: Map<String, Value> = items
                .iter()
                .map(|o| (o.clone(), Value::String(o.clone())))
                .collect();
            q.insert("type".into(), Value::String("choice".into()));
            q.insert("criteria".into(), Value::Object(map));
        }
        (None, Some(list)) => {
            let items = split_list(list, "--levels")?;
            if !(2..=10).contains(&items.len()) {
                return Err(Error::invalid(
                    "flags",
                    format!("--levels takes 2-10 values, got {}", items.len()),
                ));
            }
            q.insert("type".into(), Value::String("score".into()));
            q.insert(
                "criteria".into(),
                Value::Array(items.into_iter().map(Value::String).collect()),
            );
        }
        (None, None) => {
            q.insert("type".into(), Value::String("noul".into()));
            note(&missing_criteria("answer"));
        }
    }

    let decide = Decide {
        yes: yes_at.unwrap_or(DEFAULT_YES),
        no: no_at.unwrap_or(DEFAULT_NO),
        min_confidence: min_confidence.unwrap_or(DEFAULT_MIN_CONFIDENCE),
        validated: None,
        tuned: yes_at.is_some() || no_at.is_some() || min_confidence.is_some(),
    };
    if decide.no > decide.yes {
        return Err(Error::invalid("flags", "--no-at is above --yes-at"));
    }

    let mut wire = Map::new();
    wire.insert("answer".into(), Value::Object(q));
    Ok(Prepared {
        names: vec!["answer".into()],
        wire,
        decide: vec![decide],
        model: None,
        max_chars: None,
    })
}

fn split_list(raw: &str, flag: &str) -> Result<Vec<String>> {
    let items: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if items.is_empty() {
        return Err(Error::invalid("flags", format!("{flag} is empty")));
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_and_notes_never_reach_the_wire() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"noul","instructions":"i",
                 "decide":{"yes":0.8,"no":0.2},"notes":"measured tuesday"}}}"#,
            "test",
        )
        .expect("parses");
        let prepared = set.prepare().expect("prepares");
        let wire = prepared.wire["q"].as_object().expect("object");
        assert!(!wire.contains_key("decide"));
        assert!(!wire.contains_key("notes"));
        assert_eq!(prepared.decide[0].yes, 0.8);
    }

    #[test]
    fn an_unknown_question_field_still_travels() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"noul","instructions":"i","future":42}}}"#,
            "test",
        )
        .expect("parses");
        let prepared = set.prepare().expect("prepares");
        assert_eq!(prepared.wire["q"]["future"], 42);
    }

    #[test]
    fn a_threshold_the_wrong_way_round_is_refused() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"noul","instructions":"i",
                 "decide":{"yes":0.2,"no":0.8}}}}"#,
            "test",
        )
        .expect("parses");
        assert!(set.prepare().is_err());
    }

    fn choice_with(n: usize) -> Result<Prepared> {
        let crit: Vec<String> = (0..n).map(|i| format!(r#""o{i}":"opcion {i}""#)).collect();
        QuestionSet::parse(
            &format!(
                r#"{{"version":1,"questions":{{"q":{{"type":"choice","instructions":"i",
                   "criteria":{{{}}}}}}}}}"#,
                crit.join(",")
            ),
            "test",
        )?
        .prepare()
    }

    #[test]
    fn one_option_is_not_a_choice() {
        assert!(choice_with(1).is_err());
    }

    #[test]
    fn more_than_eight_options_is_allowed_because_the_api_allows_it() {
        // The case this was blocking: seven tappable rows plus "scroll" and "done".
        assert!(choice_with(9).is_ok());
        assert!(choice_with(255).is_ok());
    }

    #[test]
    fn past_what_the_api_takes_is_refused_here_instead_of_over_the_wire() {
        assert!(choice_with(256).is_err());
    }

    #[test]
    fn score_levels_are_still_capped_at_what_the_api_enforces() {
        // Measured: 10 levels are accepted, 11 come back
        // `Too many score levels. Must have at most 10 levels.`
        let over = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"score","instructions":"i",
               "criteria":["a","b","c","d","e","f","g","h","i","j","k"]}}}"#,
            "test",
        )
        .expect("parses");
        assert!(over.prepare().is_err());
    }

    #[test]
    fn a_typo_in_a_top_level_key_is_an_error_not_a_silent_default() {
        assert!(
            QuestionSet::parse(r#"{"version":1,"max_char":100,"questions":{}}"#, "test").is_err()
        );
    }
}
