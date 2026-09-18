//! The only file that knows the two providers apart.
//!
//! OpenRouter's shape is verified against ~1,300 real calls. TypeSafe's direct endpoint is
//! written from its documentation and has never been exercised — nobody here has a key. It
//! is kept behind the same three functions so correcting it is one edit, and `--raw` exists
//! so the first person with a key can see what actually comes back.

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    OpenRouter,
    TypeSafe,
}

impl Provider {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "openrouter" => Ok(Provider::OpenRouter),
            "typesafe" => Ok(Provider::TypeSafe),
            other => Err(Error::invalid(
                "flags",
                format!("unknown provider `{other}` (openrouter, typesafe)"),
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::OpenRouter => "openrouter",
            Provider::TypeSafe => "typesafe",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Provider::OpenRouter => "https://openrouter.ai/api/alpha/decisions",
            Provider::TypeSafe => "https://api.typesafe.ai/v1/systemone",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Provider::OpenRouter => "typesafe/jev-1.13",
            Provider::TypeSafe => "jev-latest",
        }
    }

    pub fn env_key(self) -> &'static str {
        match self {
            Provider::OpenRouter => "OPENROUTER_API_KEY",
            Provider::TypeSafe => "TYPESAFE_API_KEY",
        }
    }

    pub fn body(self, model: &str, state: &Value, questions: &Map<String, Value>) -> Value {
        json!({ "model": model, "state": state, "questions": questions })
    }
}

/// What came back, in the shape the rest of the program uses.
#[derive(Debug)]
pub struct Response {
    pub answers: Map<String, Value>,
    pub model: Option<String>,
    pub usage: Option<Value>,
}

pub fn read(body: &Value) -> Result<Response> {
    let answers = body
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            let hint = body.get("error").map(|e| e.to_string()).unwrap_or_default();
            Error::no_answer(
                "bad_response",
                format!("no `answers` in the response {hint}"),
            )
        })?
        .clone();

    Ok(Response {
        answers,
        model: body.get("model").and_then(Value::as_str).map(str::to_owned),
        usage: body.get("usage").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_response_is_read() {
        let body = json!({
            "model": "typesafe/jev-1.13-20260917",
            "answers": {"q": {"type": "noul", "noul": 0.94}},
            "usage": {"input_tokens": 409, "cost": 1.7e-5}
        });
        let r = read(&body).expect("reads");
        assert_eq!(r.model.as_deref(), Some("typesafe/jev-1.13-20260917"));
        assert!(r.answers.contains_key("q"));
    }

    #[test]
    fn a_body_with_no_answers_is_a_no_answer_not_a_panic() {
        let err = read(&json!({"error": {"message": "nope"}})).expect_err("fails");
        assert_eq!(err.code(), 4);
    }
}
