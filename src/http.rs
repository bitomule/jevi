//! One request, one retry, one deadline. Nothing here knows what a question is.
//!
//! The classification matters more than it looks: a caller that falls back to its old
//! behaviour needs to know whether it failed to *get* an answer (retryable, transient) or
//! sent something that was never going to work (it will fail again, identically).

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};

pub fn post(url: &str, key: &str, body: &Value, timeout_ms: u64) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut attempt = 0;

    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::no_answer(
                "timeout",
                format!("no reply within {timeout_ms}ms"),
            ));
        }

        match try_once(url, key, body, left) {
            Ok(v) => return Ok(v),
            Err(e) => {
                // One retry, and only while there is real time left to use it. Retrying
                // into a 200ms remainder just spends the budget twice.
                let retryable =
                    matches!(e.kind(), "connect" | "http_429" | "http_5xx" | "truncated");
                let time_left = deadline.saturating_duration_since(Instant::now());
                if attempt == 0 && retryable && time_left >= Duration::from_secs(1) {
                    attempt += 1;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

fn try_once(url: &str, key: &str, body: &Value, budget: Duration) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(budget))
        .build()
        .into();

    let response = agent
        .post(url)
        .header("Authorization", &format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .send_json(body);

    match response {
        Ok(mut r) => r
            .body_mut()
            .read_json::<Value>()
            .map_err(|e| Error::no_answer("truncated", e.to_string())),
        Err(ureq::Error::StatusCode(code)) => Err(status_error(code, None)),
        Err(e) => {
            let text = e.to_string();
            // A name that does not resolve is the network being down. Retrying that inside
            // a 2s hook budget only delays the fallback.
            let kind = if text.contains("dns") || text.contains("resolve") {
                "dns"
            } else if text.contains("timed out") || text.contains("timeout") {
                "timeout"
            } else {
                "connect"
            };
            Err(Error::no_answer(kind, text))
        }
    }
}

/// Reads a response we could not use. `detail` is the body when we managed to read one.
pub fn status_error(code: u16, detail: Option<&str>) -> Error {
    let detail = detail.unwrap_or("").trim();
    match code {
        400 if detail.contains("max_tokens_exceeded") => Error::invalid(
            "state_too_large",
            "the state is over Jev's 32k-token window; send less",
        ),
        400 => Error::invalid(
            "bad_request",
            format!("the API rejected the request: {detail}"),
        ),
        401 | 403 => Error::no_answer("auth", format!("HTTP {code}: the key was refused")),
        429 => Error::no_answer("http_429", "rate limited"),
        c if c >= 500 => Error::no_answer("http_5xx", format!("HTTP {c}")),
        c => Error::no_answer("http", format!("HTTP {c}: {detail}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_over_long_state_is_our_fault_not_the_networks() {
        let e = status_error(
            400,
            Some(r#"{"detail":{"error_type":"max_tokens_exceeded"}}"#),
        );
        assert_eq!(e.code(), 5);
        assert_eq!(e.kind(), "state_too_large");
    }

    #[test]
    fn a_refused_key_is_a_no_answer_so_callers_fall_back() {
        assert_eq!(status_error(401, None).code(), 4);
    }

    #[test]
    fn a_server_error_is_retryable_and_a_bad_request_is_not() {
        assert_eq!(status_error(503, None).kind(), "http_5xx");
        assert_eq!(status_error(400, Some("bad shape")).kind(), "bad_request");
    }
}
