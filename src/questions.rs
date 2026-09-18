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
            if let Some(c) = obj.get("criteria") {
                let c = c.as_object().ok_or_else(|| {
                    bad("noul `criteria` must be an object with `true` and `false`".into())
                })?;
                if !c.contains_key("true") || !c.contains_key("false") {
                    return Err(bad("noul `criteria` needs both `true` and `false`".into()));
                }
            }
        }
        "choice" => {
            let c = obj
                .get("criteria")
                .and_then(Value::as_object)
                .ok_or_else(|| bad("choice needs `criteria` as an object of options".into()))?;
            if !(2..=8).contains(&c.len()) {
                return Err(bad(format!("choice needs 2-8 options, got {}", c.len())));
            }
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
            if !(2..=8).contains(&items.len()) {
                return Err(Error::invalid(
                    "flags",
                    format!("--options takes 2-8 values, got {}", items.len()),
                ));
            }
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
        }
    }

    let mut decide = Decide::default();
    if let Some(v) = yes_at {
        decide.yes = v;
    }
    if let Some(v) = no_at {
        decide.no = v;
    }
    if let Some(v) = min_confidence {
        decide.min_confidence = v;
    }
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

    #[test]
    fn choice_option_count_is_checked_before_the_network() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"choice","instructions":"i",
                 "criteria":{"only":"one"}}}}"#,
            "test",
        )
        .expect("parses");
        assert!(set.prepare().is_err());
    }

    #[test]
    fn a_typo_in_a_top_level_key_is_an_error_not_a_silent_default() {
        assert!(
            QuestionSet::parse(r#"{"version":1,"max_char":100,"questions":{}}"#, "test").is_err()
        );
    }
}
