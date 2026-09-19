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
        // Without this, ureq turns a 4xx into `Error::StatusCode(code)` — a code and nothing
        // else. The body is the only place the API says *why* it refused, so throwing it
        // away made `status_error`'s `max_tokens_exceeded` branch unreachable: it matches on
        // a detail string that nothing could ever supply. Sending a state past the window
        // came back as a bare `bad_request` with an empty message instead of the
        // `state_too_large` this crate documents.
        .http_status_as_error(false)
        .build()
        .into();

    let response = agent
        .post(url)
        .header("Authorization", &format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .send_json(body);

    match response {
        Ok(mut r) if r.status().is_success() => r
            .body_mut()
            .read_json::<Value>()
            .map_err(|e| Error::no_answer("truncated", e.to_string())),
        Ok(mut r) => {
            let code = r.status().as_u16();
            let detail = r.body_mut().read_to_string().unwrap_or_default();
            Err(status_error(code, Some(&detail)))
        }
        // Kept for the case where the status still arrives as an error, so a future ureq
        // change degrades to the old behaviour rather than to a panic.
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

    /// The test above passes whether or not anything ever reaches `status_error` with a
    /// body — it hands one over itself. It did pass, for every version up to 0.2.0, while
    /// the branch it checks was unreachable in production: the agent turned a 4xx into a
    /// bare status code and the body, the only place the API says *why*, was dropped. A
    /// state past the window came back as `bad_request` with an empty message.
    ///
    /// So this one goes through `post` against a real socket, which is the path that broke.
    #[test]
    fn the_reason_for_a_refusal_survives_the_trip_through_post() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds");
        let addr = listener.local_addr().expect("has an address");

        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accepts");
            // Read just enough of the request that the client is not writing into a closed
            // socket when the reply lands.
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut socket, &mut buf);
            let body = r#"{"detail":{"error_type":"max_tokens_exceeded"}}"#;
            let reply = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut socket, reply.as_bytes());
        });

        let e = post(
            &format!("http://{addr}/decisions"),
            "test",
            &serde_json::json!({"state":"x"}),
            4000,
        )
        .expect_err("a 400 is not an answer");

        assert_eq!(e.kind(), "state_too_large");
        assert_eq!(e.code(), 5);
        let _ = server.join();
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
