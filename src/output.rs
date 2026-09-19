//! Two renderers: one for a person reading a terminal, one for a script.

use serde_json::{json, Map, Value};

use crate::decide::{Outcome, Source};

/// One line per question. A single question drops the name, because a human asking one
/// thing does not need it echoed back.
pub fn terse(names: &[String], outcomes: &[Outcome], single: bool) -> String {
    let mut s = String::new();
    for (name, o) in names.iter().zip(outcomes) {
        if !single {
            s.push_str(name);
            s.push('\t');
        }
        match (&o.label, o.number, o.confidence) {
            (Some(label), _, Some(c)) => s.push_str(&format!("{label}\t{c:.2}")),
            (Some(label), _, None) => s.push_str(label),
            (None, Some(n), _) => s.push_str(&format!("{}\t{n:.2}", o.verdict.as_str())),
            (None, None, _) => s.push_str(o.verdict.as_str()),
        }
        if let Some(w) = o.warning {
            s.push_str(&format!("\t!{w}"));
        } else if o.verdict == crate::decide::Verdict::Unsure && o.label.is_some() {
            s.push_str("\t!unsure");
        }
        s.push('\n');
    }
    s
}

pub fn document(
    names: &[String],
    outcomes: &[Outcome],
    provider: &str,
    model: Option<&str>,
    usage: Option<&Value>,
    latency_ms: u128,
) -> Value {
    let mut answers = Map::new();
    for (name, o) in names.iter().zip(outcomes) {
        let mut entry = Map::new();
        entry.insert("type".into(), json!(o.kind));
        entry.insert("verdict".into(), json!(o.verdict.as_str()));
        if let Some(l) = &o.label {
            entry.insert("label".into(), json!(l));
        }
        if let Some(n) = o.number {
            entry.insert(
                if o.kind == "score" { "score" } else { "p" }.into(),
                json!(n),
            );
        }
        if let Some(c) = o.confidence {
            entry.insert("confidence".into(), json!(c));
        }
        if let Some(w) = o.warning {
            entry.insert("warning".into(), json!(w));
        }
        // Always, not only when defaulted. A row that says nothing about its cuts cannot be
        // told apart from a row written by a version that did not record them, and the
        // reader of a stored row is exactly who this field exists for.
        entry.insert("thresholds".into(), json!(o.thresholds.source.as_str()));
        if o.thresholds.source != Source::Default {
            // Only the cuts that could have decided THIS verdict: a noul never consults
            // min_confidence, and a choice or a score never consults yes/no. Printing the
            // unused ones would invite somebody to believe they mattered.
            entry.insert(
                "cuts".into(),
                if o.kind == "noul" {
                    json!({ "yes": o.thresholds.yes, "no": o.thresholds.no })
                } else {
                    json!({ "min_confidence": o.thresholds.min_confidence })
                },
            );
        }
        answers.insert(name.clone(), Value::Object(entry));
    }

    let mut doc = Map::new();
    doc.insert("ok".into(), json!(true));
    doc.insert("provider".into(), json!(provider));
    if let Some(m) = model {
        doc.insert("model".into(), json!(m));
    }
    doc.insert("latency_ms".into(), json!(latency_ms));
    if let Some(u) = usage {
        doc.insert("usage".into(), u.clone());
    }
    doc.insert("answers".into(), Value::Object(answers));
    Value::Object(doc)
}

/// What `--soft` prints instead of failing. A caller in soft mode branches on `ok`, never on
/// the exit code — the whole point of soft mode is that the exit code is always 0.
pub fn failure(kind: &str, message: &str) -> Value {
    json!({ "ok": false, "error": { "kind": kind, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::{Thresholds, Verdict};

    fn shipped() -> Thresholds {
        Thresholds {
            source: Source::Default,
            yes: 0.9,
            no: 0.1,
            min_confidence: 0.9,
        }
    }

    fn noul(p: f64, verdict: Verdict) -> Outcome {
        Outcome {
            kind: "noul".into(),
            verdict,
            label: None,
            number: Some(p),
            confidence: None,
            warning: None,
            thresholds: shipped(),
        }
    }

    #[test]
    fn one_question_prints_without_its_name() {
        let out = terse(&["answer".into()], &[noul(0.96, Verdict::Yes)], true);
        assert_eq!(out, "yes\t0.96\n");
    }

    #[test]
    fn several_questions_are_named() {
        let names = vec!["a".to_string(), "b".to_string()];
        let out = terse(
            &names,
            &[noul(0.96, Verdict::Yes), noul(0.04, Verdict::No)],
            false,
        );
        assert_eq!(out, "a\tyes\t0.96\nb\tno\t0.04\n");
    }

    #[test]
    fn a_forced_verdict_says_which_rule_forced_it() {
        let mut o = noul(0.99, Verdict::Unsure);
        o.warning = Some("model_mismatch");
        assert!(terse(&["a".into()], &[o], true).contains("!model_mismatch"));
    }

    #[test]
    fn an_unvalidated_question_is_marked_in_the_document() {
        let o = noul(0.99, Verdict::Yes);
        let doc = document(&["a".into()], &[o], "openrouter", None, None, 1);
        assert_eq!(doc["answers"]["a"]["thresholds"], "default");
        // Nothing was moved, so there is nothing to disclose and no noise to add.
        assert!(doc["answers"]["a"].get("cuts").is_none());
    }

    #[test]
    fn a_hand_set_cut_is_disclosed_with_its_value() {
        let mut o = noul(0.71, Verdict::Yes);
        o.thresholds = Thresholds {
            source: Source::Custom,
            yes: 0.5,
            ..shipped()
        };
        let doc = document(&["a".into()], &[o], "openrouter", None, None, 1);
        assert_eq!(doc["answers"]["a"]["thresholds"], "custom");
        assert_eq!(doc["answers"]["a"]["cuts"]["yes"], 0.5);
    }

    #[test]
    fn a_choice_discloses_the_cut_that_could_have_decided_it_and_not_the_others() {
        let o = Outcome {
            kind: "choice".into(),
            verdict: Verdict::Yes,
            label: Some("bug".into()),
            number: None,
            confidence: Some(0.71),
            warning: None,
            thresholds: Thresholds {
                source: Source::Custom,
                min_confidence: 0.6,
                ..shipped()
            },
        };
        let doc = document(&["a".into()], &[o], "openrouter", None, None, 1);
        assert_eq!(doc["answers"]["a"]["cuts"]["min_confidence"], 0.6);
        assert!(doc["answers"]["a"]["cuts"].get("yes").is_none());
    }
}
