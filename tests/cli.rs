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
fn offline_config() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("jevi-offline-{}.json", std::process::id()));
    std::fs::write(
        &path,
        r#"{"openrouter":{"api_key":"test","base_url":"http://127.0.0.1:9/decisions"}}"#,
    )
    .expect("writes the fixture");
    path
}

fn run(args: &[&str], stdin: &str) -> std::process::Output {
    let mut cmd = Command::cargo_bin("jevi").expect("binary is built");
    cmd.args(args)
        .env("JEVI_CONFIG", offline_config())
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("JEVI_API_KEY")
        .env_remove("JEVI_DISABLE")
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
