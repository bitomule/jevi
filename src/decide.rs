//! Raw answer plus threshold plus provenance, in; a verdict, out. No I/O lives here.
//!
//! Why `unsure` exists at all. Jev's probability is trustworthy at the extremes and not in
//! the middle: measured over 300 real tool results, items landing between 0.2 and 0.5 were
//! claimed at 25-45% and were true 0% of the time, while everything at or above 0.9 was
//! claimed at 0.95 and was true 0.95 of the time. A two-way classifier has to call that
//! middle region something, and both answers are wrong. So it gets its own verdict and the
//! caller decides what to do with "nobody knows".
//!
//! `unsure` is an operational statement, not an epistemic one. It means the number fell
//! between the cuts *you* validated for *this* question. A noul returns no confidence, and
//! the vendor's own documentation says 0.5 does not mean uncertain, so nothing here
//! synthesises a confidence for one.
//!
//! **All of that is about a noul, and only a noul.** A `choice` and a `score` are asked a
//! different question — *which one*, *how much* — and their answer is the option and the
//! number, not a yes. Until 0.2.1 both also carried a yes/unsure read off a confidence cut,
//! and it cost correct answers: on one screen the right row was chosen 5/5 at confidence
//! 0.62-0.84, every one of them reported `unsure` under the shipped 0.9, and the caller
//! reading the verdict threw all five away. Measured against the other side, five
//! deliberate abstentions on the same screen scored 0.50-0.56 — overlapping, not separated
//! — and a run elsewhere put correct answers from 0.76 and wrong ones up to 0.88. There is
//! no cut, so jevi stopped imposing one and hands over the service's own per-option
//! probabilities instead.
//!
//! What has to keep arriving is the model declining to pick, and that is worth being exact
//! about: **the service never abstains on its own.** Asked to choose among four options
//! none of which fit, it returned one anyway, 3/3, at confidence 0.34-0.47. An abstention
//! exists only if the question offers it as an option — a "none" — and then it arrives as
//! that label, which is where it belonged all along. A `choice` therefore goes `unsure`
//! only when no option came back at all, which is a malformed answer and says so.

use serde_json::Value;

use crate::questions::Decide;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Yes,
    No,
    Unsure,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Yes => "yes",
            Verdict::No => "no",
            Verdict::Unsure => "unsure",
        }
    }

    pub fn code(self) -> i32 {
        match self {
            Verdict::Yes => 0,
            Verdict::No => 1,
            Verdict::Unsure => 3,
        }
    }
}

/// What the caller is told about one question.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub kind: String,
    pub verdict: Verdict,
    /// The label for a choice or a score; `None` for a noul.
    pub label: Option<String>,
    /// The noul probability, or the score.
    pub number: Option<f64>,
    pub confidence: Option<f64>,
    /// Set when the verdict was forced rather than measured, or when the answer fell below
    /// a confidence the caller named. A warning never decides a choice or a score.
    pub warning: Option<&'static str>,
    /// The service's own probability per option, passed through untouched. This is the
    /// material for a caller that wants its own cut, and it is the provider's number rather
    /// than one jevi invented — which is the whole difference.
    pub probabilities: Option<Value>,
    /// Where the cuts that produced this verdict came from, and what they were.
    pub thresholds: Thresholds,
}

/// The cuts this verdict was read off, and who chose them.
///
/// This exists because saying only "default" was a lie by omission: the same answer at
/// p=0.71 comes back `yes` under `--yes-at 0.5` and `unsure` without it, and the document
/// used to claim `"thresholds": "default"` in both cases. Whoever reads a stored row later
/// is the one person who cannot tell the difference, and they are who the record is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub source: Source,
    pub yes: f64,
    pub no: f64,
    pub min_confidence: f64,
    /// Whether `min_confidence` was named by the caller. On a choice or a score it decides
    /// nothing any more, so a record that printed it unconditionally would be claiming a
    /// cut had been consulted when none was.
    pub min_confidence_set: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Nothing was supplied and nothing was measured: the numbers jevi ships.
    Default,
    /// A cut was chosen by hand — a flag, or a key in a `decide` block — with no recorded
    /// measurement behind it.
    Custom,
    /// The question carries a `validated` block naming what the cuts were measured against.
    Validated,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::Custom => "custom",
            Source::Validated => "validated",
        }
    }
}

impl Thresholds {
    fn of(decide: &Decide) -> Self {
        Thresholds {
            source: if decide.validated.is_some() {
                Source::Validated
            } else if decide.tuned {
                Source::Custom
            } else {
                Source::Default
            },
            yes: decide.yes,
            no: decide.no,
            min_confidence: decide.min_confidence,
            min_confidence_set: decide.min_confidence_set,
        }
    }
}

/// The facts a threshold was validated against, as they stand for *this* call.
pub struct Provenance<'a> {
    pub model_used: Option<&'a str>,
    pub state_chars: usize,
}

/// A threshold measured against one model, at one input length, is a measurement of that
/// situation and nothing else. Rather than apply it outside those conditions and hope, the
/// verdict is forced to `unsure` and says which condition broke.
fn provenance_warning(decide: &Decide, p: &Provenance<'_>) -> Option<&'static str> {
    let v = decide.validated.as_ref()?;
    if let (Some(want), Some(got)) = (v.model.as_deref(), p.model_used) {
        // The provider may answer with a more specific build of the pinned id
        // (`typesafe/jev-1.13` -> `typesafe/jev-1.13-20260917`), which is still that model.
        if !got.starts_with(want) {
            return Some("model_mismatch");
        }
    }
    if let Some(max) = v.max_chars {
        if p.state_chars > max {
            return Some("length_mismatch");
        }
    }
    None
}

/// A confidence below the cut the caller named is worth saying, and worth nothing more.
/// It is advice printed next to a real answer, never a verdict replacing one: measured on
/// one screen, five correct choices scored 0.62-0.84 and five deliberate abstentions scored
/// 0.50-0.56, and a sixth measurement elsewhere put correct answers from 0.76 and wrong ones
/// up to 0.88. The two populations overlap, so no cut separates them.
fn low_confidence(confidence: Option<f64>, decide: &Decide) -> Option<&'static str> {
    let c = confidence?;
    (decide.min_confidence_set && c < decide.min_confidence).then_some("low_confidence")
}

pub fn outcome(answer: &Value, decide: &Decide, provenance: &Provenance<'_>) -> Outcome {
    let kind = answer
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let warning = provenance_warning(decide, provenance);
    let thresholds = Thresholds::of(decide);

    let mut out = match kind.as_str() {
        "noul" => match answer.get("noul").and_then(Value::as_f64) {
            Some(p) => Outcome {
                kind,
                verdict: if p >= decide.yes {
                    Verdict::Yes
                } else if p <= decide.no {
                    Verdict::No
                } else {
                    Verdict::Unsure
                },
                label: None,
                number: Some(p),
                confidence: None,
                warning: None,
                probabilities: None,
                thresholds,
            },
            None => missing(kind, thresholds),
        },
        "choice" => {
            let label = answer
                .get("choice")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let confidence = answer.get("confidence").and_then(Value::as_f64);
            match label {
                Some(label) => Outcome {
                    kind,
                    // The chosen option IS the answer. See the note on `low_confidence`.
                    verdict: Verdict::Yes,
                    label: Some(label),
                    number: None,
                    confidence,
                    warning: low_confidence(confidence, decide),
                    probabilities: answer.get("probabilities").cloned(),
                    thresholds,
                },
                // A shape we half understand degrades to "I don't know" rather than
                // failing the whole call: this is exactly what a provider change looks
                // like from in here. It is also the only way a choice can carry no
                // answer, because the service always picks one — see the module note.
                None => Outcome {
                    kind,
                    verdict: Verdict::Unsure,
                    label: None,
                    number: None,
                    confidence,
                    warning: Some("incomplete_answer"),
                    probabilities: answer.get("probabilities").cloned(),
                    thresholds,
                },
            }
        }
        "score" => {
            let score = answer.get("score").and_then(Value::as_f64);
            let confidence = answer.get("confidence").and_then(Value::as_f64);
            match score {
                Some(score) => {
                    let label = answer
                        .get("legend")
                        .and_then(|l| l.get(score.round().max(0.0).to_string()))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    Outcome {
                        kind,
                        // The score IS the answer, exactly as the chosen option is.
                        verdict: Verdict::Yes,
                        label,
                        number: Some(score),
                        confidence,
                        warning: low_confidence(confidence, decide),
                        probabilities: answer.get("probabilities").cloned(),
                        thresholds,
                    }
                }
                None => missing(kind, thresholds),
            }
        }
        _ => missing(kind, thresholds),
    };

    if let Some(w) = warning {
        out.verdict = Verdict::Unsure;
        out.warning = Some(w);
    }
    out
}

fn missing(kind: String, thresholds: Thresholds) -> Outcome {
    Outcome {
        kind,
        verdict: Verdict::Unsure,
        label: None,
        number: None,
        confidence: None,
        warning: Some("incomplete_answer"),
        probabilities: None,
        thresholds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::questions::Validated;
    use serde_json::json;

    fn no_provenance() -> Provenance<'static> {
        Provenance {
            model_used: None,
            state_chars: 0,
        }
    }

    #[test]
    fn noul_lands_in_the_three_bands() {
        let d = Decide {
            yes: 0.85,
            no: 0.5,
            ..Decide::default()
        };
        let at = |p: f64| outcome(&json!({"type":"noul","noul":p}), &d, &no_provenance()).verdict;
        assert_eq!(at(0.94), Verdict::Yes);
        assert_eq!(at(0.44), Verdict::No);
        assert_eq!(at(0.70), Verdict::Unsure);
    }

    #[test]
    fn a_noul_is_never_given_a_confidence() {
        let out = outcome(
            &json!({"type":"noul","noul":0.96}),
            &Decide::default(),
            &no_provenance(),
        );
        assert!(out.confidence.is_none());
    }

    #[test]
    fn a_correct_choice_under_the_shipped_confidence_is_still_the_answer() {
        // The defect this replaces, with the real case that found it: an agent asked which
        // row of a screen led to the device information, jev picked the right one 5/5 at
        // 0.62-0.84, jevi called every one `unsure` against the shipped 0.9, and the agent
        // read the verdict and threw a correct answer away. The label was there all along.
        let out = outcome(
            &json!({"type":"choice","choice":"Acerca de","confidence":0.62}),
            &Decide::default(),
            &no_provenance(),
        );
        assert_eq!(out.verdict, Verdict::Yes);
        assert_eq!(out.label.as_deref(), Some("Acerca de"));
        assert_eq!(out.confidence, Some(0.62));
        assert!(out.warning.is_none());
    }

    #[test]
    fn a_confidence_the_caller_named_is_reported_and_decides_nothing() {
        let d = Decide {
            min_confidence: 0.9,
            min_confidence_set: true,
            tuned: true,
            ..Decide::default()
        };
        let out = outcome(
            &json!({"type":"choice","choice":"bug","confidence":0.61}),
            &d,
            &no_provenance(),
        );
        assert_eq!(out.verdict, Verdict::Yes);
        assert_eq!(out.warning, Some("low_confidence"));
    }

    #[test]
    fn a_confidence_nobody_named_produces_no_advice_either() {
        let out = outcome(
            &json!({"type":"choice","choice":"bug","confidence":0.11}),
            &Decide::default(),
            &no_provenance(),
        );
        assert!(out.warning.is_none());
    }

    #[test]
    fn the_models_abstention_arrives_as_the_option_it_was_offered() {
        // Measured against the live service: asked to choose among options none of which
        // fit, it returns one anyway, 3/3 at 0.34-0.47 — it never abstains on its own. So
        // an abstention exists only as an option the question offers, and it has to reach
        // the caller as that option and not as a verdict jevi computed.
        let out = outcome(
            &json!({"type":"choice","choice":"ninguno","confidence":0.52}),
            &Decide::default(),
            &no_provenance(),
        );
        assert_eq!(out.label.as_deref(), Some("ninguno"));
        assert_eq!(out.verdict, Verdict::Yes);
    }

    #[test]
    fn a_choice_with_no_option_at_all_is_the_one_thing_left_that_is_unsure() {
        let out = outcome(
            &json!({"type":"choice","confidence":0.9}),
            &Decide::default(),
            &no_provenance(),
        );
        assert_eq!(out.verdict, Verdict::Unsure);
        assert_eq!(out.warning, Some("incomplete_answer"));
    }

    #[test]
    fn a_score_is_not_gated_by_a_confidence_either() {
        // Same defect, same shape: `--levels` asks how much, and the number is the answer.
        let d = Decide {
            min_confidence: 0.9,
            min_confidence_set: true,
            tuned: true,
            validated: Some(Validated::default()),
            ..Decide::default()
        };
        let out = outcome(
            &json!({"type":"score","score":2.0,"confidence":0.4}),
            &d,
            &no_provenance(),
        );
        assert_eq!(out.verdict, Verdict::Yes);
        assert_eq!(out.number, Some(2.0));
        assert_eq!(out.warning, Some("low_confidence"));
    }

    #[test]
    fn the_per_option_probabilities_are_passed_through_untouched() {
        let out = outcome(
            &json!({"type":"choice","choice":"bug","confidence":0.6,
                    "probabilities":{"bug":0.6,"feature":0.4}}),
            &Decide::default(),
            &no_provenance(),
        );
        assert_eq!(out.probabilities.expect("passed through")["bug"], 0.6);
    }

    #[test]
    fn a_score_reads_its_label_off_the_legend() {
        let out = outcome(
            &json!({"type":"score","score":2.57,"confidence":0.57,
                    "legend":{"0":"trivial","1":"minor","2":"major","3":"blocking"}}),
            &Decide::default(),
            &no_provenance(),
        );
        assert_eq!(out.label.as_deref(), Some("blocking"));
        assert_eq!(out.number, Some(2.57));
    }

    #[test]
    fn a_threshold_validated_on_another_model_does_not_get_applied() {
        let d = Decide {
            yes: 0.85,
            no: 0.5,
            validated: Some(Validated {
                model: Some("typesafe/jev-1.13".into()),
                max_chars: None,
            }),
            ..Decide::default()
        };
        let p = Provenance {
            model_used: Some("typesafe/jev-2.0"),
            state_chars: 0,
        };
        let out = outcome(&json!({"type":"noul","noul":0.99}), &d, &p);
        assert_eq!(out.verdict, Verdict::Unsure);
        assert_eq!(out.warning, Some("model_mismatch"));
    }

    #[test]
    fn a_dated_build_of_the_pinned_model_still_counts_as_that_model() {
        let d = Decide {
            validated: Some(Validated {
                model: Some("typesafe/jev-1.13".into()),
                max_chars: None,
            }),
            ..Decide::default()
        };
        let p = Provenance {
            model_used: Some("typesafe/jev-1.13-20260917"),
            state_chars: 0,
        };
        let out = outcome(&json!({"type":"noul","noul":0.99}), &d, &p);
        assert_eq!(out.verdict, Verdict::Yes);
        assert!(out.warning.is_none());
    }

    #[test]
    fn a_longer_state_than_was_measured_downgrades_the_verdict() {
        let d = Decide {
            validated: Some(Validated {
                model: None,
                max_chars: Some(1000),
            }),
            ..Decide::default()
        };
        let p = Provenance {
            model_used: None,
            state_chars: 4000,
        };
        let out = outcome(&json!({"type":"noul","noul":0.99}), &d, &p);
        assert_eq!(out.verdict, Verdict::Unsure);
        assert_eq!(out.warning, Some("length_mismatch"));
    }

    #[test]
    fn an_answer_missing_its_field_is_unsure_not_a_failed_call() {
        let out = outcome(
            &json!({"type":"choice"}),
            &Decide::default(),
            &no_provenance(),
        );
        assert_eq!(out.verdict, Verdict::Unsure);
        assert_eq!(out.warning, Some("incomplete_answer"));
    }

    #[test]
    fn a_question_nobody_validated_says_so() {
        let out = outcome(
            &json!({"type":"noul","noul":0.99}),
            &Decide::default(),
            &no_provenance(),
        );
        assert_eq!(out.thresholds.source, Source::Default);
    }

    #[test]
    fn a_cut_moved_by_hand_is_custom_and_not_default() {
        // The exact case that made the record a lie: p=0.71 is `yes` under a hand-set cut of
        // 0.5 and `unsure` under the shipped 0.9, and both used to record "default".
        let answer = json!({"type":"noul","noul":0.71});
        let tuned = Decide {
            yes: 0.5,
            tuned: true,
            ..Decide::default()
        };

        let out = outcome(&answer, &tuned, &no_provenance());
        assert_eq!(out.verdict, Verdict::Yes);
        assert_eq!(out.thresholds.source, Source::Custom);
        assert_eq!(out.thresholds.yes, 0.5);

        let out = outcome(&answer, &Decide::default(), &no_provenance());
        assert_eq!(out.verdict, Verdict::Unsure);
        assert_eq!(out.thresholds.source, Source::Default);
    }

    #[test]
    fn a_validated_block_outranks_a_hand_set_cut() {
        let d = Decide {
            yes: 0.8,
            tuned: true,
            validated: Some(Validated {
                model: None,
                max_chars: None,
            }),
            ..Decide::default()
        };
        let out = outcome(&json!({"type":"noul","noul":0.99}), &d, &no_provenance());
        assert_eq!(out.thresholds.source, Source::Validated);
        assert_eq!(out.thresholds.yes, 0.8);
    }
}
