// An integration test is its own crate, so clippy's allow-*-in-tests does not reach it.
// `expect` here is an assertion with a message, which is exactly what a test wants.
#![allow(clippy::expect_used)]

//! End-to-end checks of the contract a caller depends on: the exit codes, and the promise
//! that a hook is never broken by this binary. None of these touch the network — they run
//! on a config whose `base_url` points at a closed port, which is what "the API is down"
//! looks like from in here.

use std::io::Write;
use std::process::{Command, Stdio};

use assert_cmd::prelude::*;

/// A config pointing at a port nothing listens on: every call fails to connect, fast.
///
/// Written exactly once. Cargo runs these tests as threads of ONE process, so a path keyed
/// on the pid is shared by all of them — and every test rewriting it meant one could read it
/// mid-write, get invalid JSON, and see exit 5 (bad config) where it expected 4 or 0. On
/// macOS the race never landed; on Linux CI two tests failed every run.
fn offline_config() -> &'static std::path::Path {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let path = std::env::temp_dir().join(format!("jevi-offline-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"openrouter":{"api_key":"test","base_url":"http://127.0.0.1:9/decisions"}}"#,
        )
        .expect("writes the fixture");
        path
    })
}

fn run(args: &[&str], stdin: &str) -> std::process::Output {
    let mut cmd = Command::cargo_bin("jevi").expect("binary is built");
    cmd.args(args)
        .env("JEVI_CONFIG", offline_config())
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("JEVI_API_KEY")
        .env_remove("JEVI_DISABLE")
        .env_remove("JEVI_QUIET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawns");
    // A broken pipe here is the binary doing its job, not a failure: several of these cases
    // are refused before stdin is ever read (a bad flag, a missing question set), so the
    // child can be gone before the write lands. It is a race, and on a fast machine the
    // write usually wins — this only showed up on CI. Everything else is still an error.
    if let Err(e) = child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
    {
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe,
            "writing stdin failed for a reason other than the child exiting early"
        );
    }
    child.wait_with_output().expect("runs")
}

fn code(out: &std::process::Output) -> i32 {
    out.status.code().expect("exited normally")
}

#[test]
fn a_mistyped_flag_exits_5_and_never_2() {
    // Claude Code reads a hook's exit 2 as "block this tool call". clap's own default for a
    // bad flag is 2, so this is the test that keeps `try_parse` in main.
    let out = run(&["ask", "is this urgent", "--nope"], "");
    assert_eq!(
        code(&out),
        5,
        "a bad flag must be invalid input, not a block"
    );
}

#[test]
fn help_and_version_are_a_success() {
    assert_eq!(code(&run(&["--help"], "")), 0);
    assert_eq!(code(&run(&["--version"], "")), 0);
}

#[test]
fn an_unreachable_api_is_exit_4_so_a_caller_can_fall_back() {
    let out = run(
        &["ask", "is this urgent", "--timeout", "1200"],
        "the server is down",
    );
    assert_eq!(code(&out), 4);
    assert!(String::from_utf8_lossy(&out.stderr).contains("no answer"));
}

#[test]
fn soft_turns_every_failure_into_exit_0_and_a_json_reason() {
    let out = run(
        &["ask", "is this urgent", "--soft", "--timeout", "1200"],
        "the server is down",
    );
    assert_eq!(code(&out), 0, "a hook must never be broken by this binary");
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("soft mode always prints a JSON document");
    assert_eq!(doc["ok"], false);
    assert!(doc["error"]["kind"].is_string());
    assert!(out.stderr.is_empty(), "soft mode stays quiet on stderr");
}

#[test]
fn an_empty_state_is_refused_without_a_round_trip() {
    let out = run(&["ask", "is this urgent"], "   \n  ");
    assert_eq!(code(&out), 5);
}

#[test]
fn a_question_set_that_does_not_exist_names_where_it_looked() {
    let out = run(&["ask", "-f", "nope"], "text");
    assert_eq!(code(&out), 5);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(".jevi"),
        "the error should say where it looked: {err}"
    );
}

#[test]
fn a_malformed_question_file_is_invalid_input_not_a_failed_call() {
    let path = std::env::temp_dir().join(format!("jevi-bad-{}.json", std::process::id()));
    std::fs::write(&path, r#"{"version":1,"questions":{"q":{"type":"nope"}}}"#).expect("writes");
    let out = run(&["ask", "-f", path.to_str().expect("utf8")], "text");
    assert_eq!(code(&out), 5);
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_question_and_a_set_together_is_refused() {
    let out = run(&["ask", "is this urgent", "-f", "whatever"], "text");
    assert_eq!(code(&out), 5);
}

/// `--questions-json`, and the reason it exists.
///
/// A caller whose options change on every call — one candidate list per screen — had no way
/// to say so: `-f` takes a path or a name, so the only route was writing a temporary file
/// per call inside a hot loop. These run offline, because what is being checked is that the
/// question is accepted and reaches the point of being sent, and exit 4 is the API being
/// unreachable rather than the question being wrong.
mod questions_json {
    use super::*;

    const SET: &str = r#"{"version":1,"questions":{"answer":{"type":"choice",
        "instructions":{"goal":"the settings button","rules":["answer none if absent"]},
        "criteria":{"1":{"role":"button","name":"Ajustes"},"none":"none"}}}}"#;

    #[test]
    fn a_question_set_inline_is_accepted_and_sent() {
        let out = run(
            &["ask", "--questions-json", SET],
            "1) label=Ajustes role=button",
        );
        assert_eq!(code(&out), 4, "{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn the_set_can_come_from_stdin_with_the_state_on_a_flag() {
        let out = run(
            &[
                "ask",
                "--questions-json",
                "-",
                "--text",
                "1) label=Ajustes role=button",
            ],
            SET,
        );
        assert_eq!(code(&out), 4, "{}", String::from_utf8_lossy(&out.stderr));
    }

    /// Both cannot come from stdin, and saying so beats parsing the set out of the state.
    #[test]
    fn the_set_on_stdin_without_a_state_flag_is_refused() {
        let out = run(&["ask", "--questions-json", "-"], SET);
        assert_eq!(code(&out), 5);
        assert!(String::from_utf8_lossy(&out.stderr).contains("--text"));
    }

    #[test]
    fn two_sources_for_one_question_is_refused() {
        for args in [
            vec!["ask", "is this urgent", "--questions-json", SET],
            vec!["ask", "--questions-json", SET, "-f", "whatever"],
        ] {
            let out = run(&args, "text");
            assert_eq!(code(&out), 5, "accepted two question sources: {args:?}");
        }
    }

    /// Invalid input, not a failed call: this request will be wrong again, so a caller that
    /// degrades on exit 4 must not degrade on this.
    #[test]
    fn a_broken_set_is_exit_5_and_names_the_flag() {
        let out = run(&["ask", "--questions-json", "{nope"], "text");
        assert_eq!(code(&out), 5);
        assert!(String::from_utf8_lossy(&out.stderr).contains("--questions-json"));
    }
}

#[test]
fn the_disable_switch_works_without_editing_the_caller() {
    let mut cmd = Command::cargo_bin("jevi").expect("binary is built");
    let out = cmd
        .args(["ask", "is this urgent", "--soft"])
        .env("JEVI_DISABLE", "1")
        .env("JEVI_CONFIG", offline_config())
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(0));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(doc["error"]["kind"], "disabled");
}

#[test]
fn doctor_reports_without_a_key_instead_of_failing() {
    let out = run(&["doctor"], "");
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("key:"));
}

/// The criteria note, and the control that proves it is not simply always on.
///
/// A `noul` with no `criteria` stays legal — the API takes it, so jevi takes it — but a
/// state carrying its own instructions flips the verdict far more often without them, and
/// saying nothing about that was the defect. These run offline: the note is written while
/// the question is prepared, before anything is sent.
mod criteria_note {
    use super::*;

    fn stderr_of(args: &[&str], env: &[(&str, &str)]) -> String {
        let mut cmd = Command::cargo_bin("jevi").expect("binary is built");
        cmd.args(args)
            .env("JEVI_CONFIG", offline_config())
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("TYPESAFE_API_KEY")
            .env_remove("JEVI_API_KEY")
            .env_remove("JEVI_DISABLE")
            .env_remove("JEVI_QUIET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawns");
        let _ = child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(b"the server is down");
        let out = child.wait_with_output().expect("runs");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    #[test]
    fn a_noul_without_criteria_is_answered_but_says_so() {
        let err = stderr_of(&["ask", "is this urgent", "--timeout", "1200"], &[]);
        assert!(
            err.contains("no `criteria`"),
            "expected the note, got: {err}"
        );
    }

    #[test]
    fn a_question_that_already_has_criteria_gets_no_note() {
        // The control. `--options` builds a choice whose options ARE its criteria, so if
        // this ever prints the note, the note fires on everything and means nothing.
        let err = stderr_of(
            &[
                "ask",
                "how urgent",
                "--options",
                "low,high",
                "--timeout",
                "1200",
            ],
            &[],
        );
        assert!(
            !err.contains("no `criteria`"),
            "the note fired on a question that has criteria: {err}"
        );
    }

    #[test]
    fn the_note_can_be_silenced_for_a_caller_that_runs_in_a_loop() {
        let err = stderr_of(
            &["ask", "is this urgent", "--timeout", "1200"],
            &[("JEVI_QUIET", "1")],
        );
        assert!(
            !err.contains("no `criteria`"),
            "JEVI_QUIET was ignored: {err}"
        );
    }
}
