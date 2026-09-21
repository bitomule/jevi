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
    if len == 0 {
        return Err(Error::invalid(
            "question_file",
            format!("question `{name}`: choice needs at least one option, got none"),
        ));
    }
    // One option was refused here for the same reason 9 used to be, and it is the same
    // mistake: measured against the endpoint, a choice with a single option is accepted and
    // answered. It is a useless question — it can only ever return that option — but a
    // caller building its options per call from whatever is on the screen will hit a screen
    // with one candidate, and exit 5 "invalid input" is a worse answer than the answer.
    if len == 1 {
        note(&format!(
            "question `{name}` offers one option, so it can only return that option. The \
             service answers it; nothing is being decided. Set JEVI_QUIET=1 to silence this."
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

/// The service calls this an `EntryType`, and it is the same rule in four places:
/// `instructions`, a Choice option's description, a Score level, and a Noul's `true` and
/// `false`. The documentation says all four accept `string`, `object`, `array` **or
/// `null``**, and measured against the endpoint that last one is only true of a Choice
/// option. `instructions: null`, a `null` among Score levels, and `criteria.true: null`
/// each come back 400.
///
/// So the flag exists because the docs are wrong about three of the four, and the numbers
/// and booleans are refused everywhere. Checked here rather than over the wire: a 400 is an
/// `Error::NoAnswer` to a caller that cannot see inside it, which reads as "the service was
/// unreachable, carry on degraded" — when the truth is a malformed request that will be
/// malformed again.
fn check_entry(name: &str, field: &str, v: &Value, null_ok: bool) -> Result<()> {
    let ok = match v {
        Value::String(_) | Value::Object(_) | Value::Array(_) => true,
        Value::Null => null_ok,
        Value::Bool(_) | Value::Number(_) => false,
    };
    if ok {
        return Ok(());
    }
    let allowed = if null_ok {
        "a string, an object, an array or null"
    } else {
        "a string, an object or an array"
    };
    Err(Error::invalid(
        "question_file",
        format!("question `{name}`: {field} must be {allowed}"),
    ))
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
    /// True when `min_confidence` specifically was supplied. Tracked apart from `tuned`
    /// because a `--yes-at` on a choice question must not make every answer under the
    /// shipped 0.9 report a confidence nobody asked about.
    pub min_confidence_set: bool,
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
            min_confidence_set: false,
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

    // `instructions` is an `EntryType` — a string, an object or an array, all three sent
    // untouched. It was a string and nothing else here, enforced by an `as_str` and by
    // nothing in the service, and that refusal had a measured price: on one recorded cell of
    // mav's ablation bench the same question with the same options and the same state scored
    // 26/30 with `instructions` as an object and 6/30 with its own words flattened into a
    // string. Both clients written against this endpoint natively send the object.
    //
    // An empty string and an empty object are answered by the service, so they are a note
    // and not a refusal — same rule as the option count above. `null`, a missing field and
    // any scalar are refused, because the service refuses them.
    match obj.get("instructions") {
        Some(v) => {
            check_entry(name, "`instructions`", v, false)?;
            let empty = match v {
                Value::String(s) => s.trim().is_empty(),
                Value::Object(o) => o.is_empty(),
                Value::Array(a) => a.is_empty(),
                _ => false,
            };
            if empty {
                note(&format!(
                    "question `{name}` has empty `instructions`, so the only thing saying what \
                     is being asked is the options. The service answers it. Set JEVI_QUIET=1 to \
                     silence this."
                ));
            }
        }
        None => return Err(bad("missing `instructions`".into())),
    }

    // The criteria rules are the API's, checked here so a typo costs nothing instead of a
    // round trip and a 400 — and a 400 does not even reach the caller as invalid input, it
    // reaches it as "no answer", which is the code that means "carry on degraded".
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
                    // Extra keys beside the two are accepted by the service, so they pass.
                    for key in ["true", "false"] {
                        if let Some(v) = c.get(key) {
                            check_entry(name, &format!("noul `criteria.{key}`"), v, false)?;
                        }
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
            // The one place `null` really is allowed, and it is the documented way to say
            // "the key is the whole option": `{"Beaver Dam Logistics": null, …}`.
            for (option, v) in c {
                check_entry(name, &format!("choice option `{option}`"), v, true)?;
            }
        }
        "score" => {
            let c = obj
                .get("criteria")
                .and_then(Value::as_array)
                .ok_or_else(|| bad("score needs `criteria` as an array of levels".into()))?;
            // 1 level is accepted and answered by the service — uselessly, since the answer
            // can only be that level — so it is a note. 11 come back
            // `Too many score levels. Must have at most 10 levels.`
            if c.is_empty() {
                return Err(bad("score needs at least one level, got none".into()));
            }
            if c.len() > 10 {
                return Err(bad(format!(
                    "score takes at most 10 levels, got {}",
                    c.len()
                )));
            }
            if c.len() == 1 {
                note(&format!(
                    "question `{name}` offers one score level, so it can only return that \
                     level. Set JEVI_QUIET=1 to silence this."
                ));
            }
            for (i, v) in c.iter().enumerate() {
                check_entry(name, &format!("score level {i}"), v, false)?;
            }
        }
        other => return Err(bad(format!("unknown type `{other}`"))),
    }

    let decide = parse_decide(name, obj.get("decide"))?;

    // Everything except our two private keys travels to the API untouched, so a field this
    // build has never heard of still reaches the model.
    //
    // "Untouched" is load-bearing and it was not true until `serde_json`'s `preserve_order`
    // was turned on in Cargo.toml. A `BTreeMap` re-sorts the keys of every object it parses,
    // so this loop faithfully cloned values into a question whose ORDER had already been
    // rewritten: `{goal, context, rules}` left as `{context, goal, rules}`. Measured on one
    // recorded cell of mav's ablation bench, 30 runs each through this binary, everything
    // else identical: 2/30 correct sorted, 29/30 in the order the caller wrote. If that
    // feature is ever dropped, this comment becomes a lie again and nothing will fail.
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
    let min_confidence_set = obj.contains_key("min_confidence");

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
        min_confidence_set,
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
            // The service's own bounds, so this flag refuses exactly what a question set
            // refuses: 10 levels are accepted and 11 come back `Too many score levels`.
            if items.len() > 10 {
                return Err(Error::invalid(
                    "flags",
                    format!("--levels takes at most 10 values, got {}", items.len()),
                ));
            }
            if items.len() == 1 {
                note(
                    "--levels was given one level, so the score can only be that level. \
                     Set JEVI_QUIET=1 to silence this.",
                );
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
        min_confidence_set: min_confidence.is_some(),
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

    /// The guard on `serde_json`'s `preserve_order`, and it is a real guard rather than a
    /// tautology: with that feature off this test fails and the tool silently gets worse.
    ///
    /// A `BTreeMap` re-sorts the keys of every object it parses, so a question written
    /// `{goal, context, rules}` reached the model as `{context, goal, rules}` and an option
    /// written `{role, name, id}` as `{id, name, role}` — while the source claimed the
    /// question travelled untouched. Measured on one recorded cell of mav's ablation bench,
    /// 30 runs each through the binary, same words, same options, same state: 2/30 correct
    /// with the keys sorted, 29/30 in the order the caller wrote them. The order was worth
    /// 27 of 30, so it is part of the question and not formatting.
    #[test]
    fn the_order_the_caller_wrote_the_keys_in_survives() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"choice",
                 "instructions":{"goal":"g","context":"c","rules":["r"]},
                 "criteria":{"1":{"role":"button","name":"Ajustes","id":"settingsButton"},
                             "none":"none"}}}}"#,
            "test",
        )
        .expect("parses");
        let prepared = set.prepare().expect("prepares");

        let keys = |v: &Value| -> Vec<String> {
            v.as_object()
                .expect("object")
                .keys()
                .map(String::clone)
                .collect()
        };
        assert_eq!(
            keys(&prepared.wire["q"]["instructions"]),
            ["goal", "context", "rules"],
            "the keys of `instructions` were re-sorted on the way through"
        );
        assert_eq!(
            keys(&prepared.wire["q"]["criteria"]["1"]),
            ["role", "name", "id"],
            "the keys of an option's record were re-sorted on the way through"
        );

        // And once more through serialisation, which is what actually goes on the wire.
        let body = serde_json::to_string(&prepared.wire).expect("serialises");
        let goal = body.find(r#""goal""#).expect("goal is in the body");
        let context = body.find(r#""context""#).expect("context is in the body");
        assert!(
            goal < context,
            "serialising put the keys back in sorted order"
        );
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

    /// One option used to be refused, for the same reason nine used to be, and it was the
    /// same mistake: measured against the endpoint, a single-option choice is accepted and
    /// answered. A caller building its options per call will meet a screen with one
    /// candidate, and `exit 5 invalid input` is a worse answer than the answer. It still
    /// decides nothing, so it gets a note.
    #[test]
    fn one_option_is_a_pointless_choice_and_not_a_refused_one() {
        assert!(choice_with(1).is_ok());
    }

    #[test]
    fn no_options_at_all_is_still_refused() {
        assert!(choice_with(0).is_err());
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

    /// `instructions` as an object, which is what the two clients written against the
    /// endpoint natively send. jevi refused it for nothing but an `as_str`, and the cost of
    /// that refusal is measured: over 150 runs on one recorded cell of mav's ablation bench,
    /// the same question with the same structured options and the same state scored 26/30
    /// with the object and 6/30 with its own words flattened into a string. The container
    /// was the single largest of the three pieces that cell needed.
    #[test]
    fn instructions_can_be_an_object_and_it_travels_whole() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"choice",
                 "instructions":{"goal":"the settings button","rules":["answer none if absent"]},
                 "criteria":{"1":{"role":"button","name":"Ajustes"},"none":"none"}}}}"#,
            "test",
        )
        .expect("parses");
        let prepared = set.prepare().expect("prepares");
        assert_eq!(
            prepared.wire["q"]["instructions"]["goal"],
            "the settings button"
        );
        assert_eq!(
            prepared.wire["q"]["instructions"]["rules"][0],
            "answer none if absent"
        );
    }

    /// The other half of the same change: an option whose value is its structured record
    /// passes through untouched, rather than being flattened to the key's own name.
    #[test]
    fn a_choice_option_can_carry_its_own_record() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"choice","instructions":"i",
                 "criteria":{"1":{"role":"button","name":"Ajustes","id":"settingsButton"},
                             "none":"none"}}}}"#,
            "test",
        )
        .expect("parses");
        let prepared = set.prepare().expect("prepares");
        assert_eq!(prepared.wire["q"]["criteria"]["1"]["id"], "settingsButton");
    }

    #[test]
    fn instructions_that_are_neither_a_string_nor_an_object_are_refused_here() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"noul","instructions":42}}}"#,
            "test",
        )
        .expect("parses");
        assert!(set.prepare().is_err());
    }

    /// The rule this file has stated since the nine-options mistake, applied again: refuse
    /// what the service refuses and warn about the rest. Measured, one call each: an empty
    /// string and an empty object both come back 200 and answered, so they are a note.
    #[test]
    fn an_empty_instructions_is_answered_by_the_service_so_it_is_not_refused_here() {
        for raw in [
            r#"{"version":1,"questions":{"q":{"type":"noul","instructions":"  ","criteria":{"true":"a","false":"b"}}}}"#,
            r#"{"version":1,"questions":{"q":{"type":"noul","instructions":{},"criteria":{"true":"a","false":"b"}}}}"#,
            r#"{"version":1,"questions":{"q":{"type":"noul","instructions":[],"criteria":{"true":"a","false":"b"}}}}"#,
        ] {
            let set = QuestionSet::parse(raw, "test").expect("parses");
            assert!(
                set.prepare().is_ok(),
                "refused what the service answers: {raw}"
            );
        }
    }

    /// `instructions` as an array, the third shape the service takes. jev-ultrafast uses a
    /// list where it has two rule sets to send (`"rules": [NEXT_ACTION, TARGET]`), and the
    /// docs give the same example for "the instruction is a list of things to check".
    #[test]
    fn instructions_can_be_an_array() {
        let set = QuestionSet::parse(
            r#"{"version":1,"questions":{"q":{"type":"choice",
                 "instructions":["which team handles this","classify the primary request"],
                 "criteria":{"billing":null,"orders":null}}}}"#,
            "test",
        )
        .expect("parses");
        let prepared = set.prepare().expect("prepares");
        assert_eq!(
            prepared.wire["q"]["instructions"][1],
            "classify the primary request"
        );
    }

    /// The documentation says `null` is an accepted shape in all four places an `EntryType`
    /// appears. Measured against the endpoint it is true in exactly one of them: a Choice
    /// option. `instructions: null`, a `null` among Score levels and `criteria.true: null`
    /// each come back 400, and a 400 reaches a caller as "no answer" — the code that means
    /// "carry on degraded" — so refusing them here is the difference between a defect being
    /// surfaced and a defect being shrugged off.
    #[test]
    fn null_is_only_allowed_where_the_service_allows_it_not_where_the_docs_say() {
        let ok = r#"{"version":1,"questions":{"q":{"type":"choice","instructions":"which",
                     "criteria":{"Beaver Dam Logistics":null,"Beaver":null}}}}"#;
        assert!(QuestionSet::parse(ok, "test")
            .expect("parses")
            .prepare()
            .is_ok());

        for raw in [
            r#"{"version":1,"questions":{"q":{"type":"choice","instructions":null,"criteria":{"a":null,"b":null}}}}"#,
            r#"{"version":1,"questions":{"q":{"type":"score","instructions":"how big","criteria":["a",null,"c"]}}}"#,
            r#"{"version":1,"questions":{"q":{"type":"noul","instructions":"is it","criteria":{"true":null,"false":"no"}}}}"#,
            r#"{"version":1,"questions":{"q":{"type":"choice","instructions":"which","criteria":{"a":1,"b":2}}}}"#,
        ] {
            let set = QuestionSet::parse(raw, "test").expect("parses");
            assert!(
                set.prepare().is_err(),
                "accepted what the service answers with a 400: {raw}"
            );
        }
    }

    #[test]
    fn a_typo_in_a_top_level_key_is_an_error_not_a_silent_default() {
        assert!(
            QuestionSet::parse(r#"{"version":1,"max_char":100,"questions":{}}"#, "test").is_err()
        );
    }
}
