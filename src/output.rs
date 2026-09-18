//! Two renderers: one for a person reading a terminal, one for a script.

use serde_json::{json, Map, Value};

use crate::decide::Outcome;

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
        if o.defaulted {
            entry.insert("thresholds".into(), json!("default"));
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
    use crate::decide::Verdict;

    fn noul(p: f64, verdict: Verdict) -> Outcome {
        Outcome {
            kind: "noul".into(),
            verdict,
            label: None,
            number: Some(p),
            confidence: None,
            warning: None,
            defaulted: false,
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
        let mut o = noul(0.99, Verdict::Yes);
        o.defaulted = true;
        let doc = document(&["a".into()], &[o], "openrouter", None, None, 1);
        assert_eq!(doc["answers"]["a"]["thresholds"], "default");
    }
}
