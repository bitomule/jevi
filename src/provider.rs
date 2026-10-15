//! The only file that knows the two providers apart.
//!
//! OpenRouter's shape is verified against ~1,300 real calls. TypeSafe's direct endpoint is
//! written from its documentation and has never been exercised — nobody here has a key. It
//! is kept behind the same three functions so correcting it is one edit, and `--raw` exists
//! so the first person with a key can see what actually comes back.
//!
//! Images are where the two differ most, and only OpenRouter's half is verified. TypeSafe
//! documents a top-level `assets` map, a reference string standing in for each image
//! wherever it is used, and a `TypeSafe-Danger-Zone-Images` header. OpenRouter takes none
//! of that: sent `assets`, it answered without error and billed 281 input tokens, the same
//! as the text alone — the image was dropped in silence. What it does take is an
//! `image_url` object holding a data URL, placed in the state like any other value: the
//! same question then billed 3,226 tokens and answered about the picture.

use std::hash::{BuildHasher, Hasher};

use base64::Engine;
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::image::Image;

/// What goes on the wire for one call.
#[derive(Debug)]
pub struct Request {
    pub body: Value,
    pub headers: Vec<(&'static str, &'static str)>,
}

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

    /// The first model that answers a `box`. 1.13 refuses one with a bare 400.
    pub fn box_model(self) -> &'static str {
        match self {
            Provider::OpenRouter => "typesafe/jev-1.14",
            Provider::TypeSafe => "jev-1.14.0",
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

    /// The body and headers for one call. With images, the state becomes an object holding
    /// each image under its name, and the text, if there is one, under `text`.
    pub fn request(
        self,
        model: &str,
        state: &Value,
        questions: &Map<String, Value>,
        images: &[Image],
        has_box: bool,
    ) -> Result<Request> {
        let mut assets = Map::new();
        let state = if images.is_empty() {
            state.clone()
        } else {
            let mut obj = match state {
                Value::Null => Map::new(),
                Value::String(s) => Map::from_iter([("text".to_owned(), Value::String(s.clone()))]),
                Value::Object(o) => o.clone(),
                _ => {
                    return Err(Error::invalid(
                        "image",
                        "images go in a state that is text or a JSON object, and this one is \
                         neither",
                    ))
                }
            };
            for img in images {
                if obj.contains_key(&img.name) {
                    return Err(Error::invalid(
                        "image",
                        format!(
                            "the state already has a `{}`; name the image something else",
                            img.name
                        ),
                    ));
                }
                let value = match self {
                    Provider::OpenRouter => image_url(img),
                    Provider::TypeSafe => {
                        let reference = reference(&img.name);
                        assets.insert(
                            reference.clone(),
                            json!({
                                "type": "image",
                                "format": img.format.name(),
                                "data_base64": base64::engine::general_purpose::STANDARD.encode(&img.bytes),
                            }),
                        );
                        Value::String(reference)
                    }
                };
                obj.insert(img.name.clone(), value);
            }
            Value::Object(obj)
        };

        let mut body = self.body(model, &state, questions);
        let mut headers = Vec::new();
        if self == Provider::TypeSafe {
            if !assets.is_empty() {
                body["assets"] = Value::Object(assets);
            }
            if !images.is_empty() || has_box {
                headers.push(("TypeSafe-Danger-Zone-Images", "true"));
            }
        }
        Ok(Request { body, headers })
    }
}

fn image_url(img: &Image) -> Value {
    let data = base64::engine::general_purpose::STANDARD.encode(&img.bytes);
    json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{};base64,{data}", img.format.mime()) }
    })
}

/// TypeSafe replaces a reference wherever it appears in the request, inside any string, so
/// one that could occur in somebody's text would swap their words for a picture. The
/// documentation's advice is a UUID; this is the image's name plus 64 random bits.
fn reference(name: &str) -> String {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write(name.as_bytes());
    if let Ok(t) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        h.write_u128(t.as_nanos());
    }
    format!("jevi-image-{name}-{:016x}", h.finish())
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

    fn screen() -> Image {
        Image::from_bytes("screen", crate::image::tests::png(300, 600)).expect("an image")
    }

    fn one_question() -> Map<String, Value> {
        Map::from_iter([("q".to_owned(), json!({"type": "noul", "instructions": "i"}))])
    }

    #[test]
    fn without_images_the_request_is_the_one_jevi_always_sent() {
        for p in [Provider::OpenRouter, Provider::TypeSafe] {
            let r = p
                .request("m", &json!("text"), &one_question(), &[], false)
                .expect("builds");
            assert_eq!(r.body, p.body("m", &json!("text"), &one_question()));
            assert!(r.headers.is_empty());
        }
    }

    #[test]
    fn openrouter_gets_the_image_as_a_data_url_under_its_name() {
        let r = Provider::OpenRouter
            .request("m", &json!("the text"), &one_question(), &[screen()], false)
            .expect("builds");
        assert_eq!(r.body["state"]["text"], "the text");
        assert_eq!(r.body["state"]["screen"]["type"], "image_url");
        let url = r.body["state"]["screen"]["image_url"]["url"]
            .as_str()
            .expect("a url");
        assert!(url.starts_with("data:image/png;base64,iVBOR"), "{url:.40}");
        assert!(
            r.body.get("assets").is_none(),
            "OpenRouter drops `assets` in silence"
        );
        assert!(r.headers.is_empty());
    }

    #[test]
    fn typesafe_gets_a_reference_an_asset_and_the_header() {
        let r = Provider::TypeSafe
            .request("m", &Value::Null, &one_question(), &[screen()], false)
            .expect("builds");
        let reference = r.body["state"]["screen"].as_str().expect("a reference");
        assert!(reference.starts_with("jevi-image-screen-"));
        let asset = &r.body["assets"][reference];
        assert_eq!(asset["format"], "png");
        assert!(asset["data_base64"]
            .as_str()
            .is_some_and(|d| d.starts_with("iVBOR")));
        assert_eq!(r.headers, vec![("TypeSafe-Danger-Zone-Images", "true")]);
    }

    #[test]
    fn a_box_question_needs_the_header_even_without_an_image_in_this_call() {
        let r = Provider::TypeSafe
            .request("m", &json!("t"), &one_question(), &[], true)
            .expect("builds");
        assert_eq!(r.headers, vec![("TypeSafe-Danger-Zone-Images", "true")]);
    }

    #[test]
    fn two_references_to_the_same_name_never_collide() {
        assert_ne!(reference("screen"), reference("screen"));
    }

    #[test]
    fn an_image_joins_a_json_object_state_and_never_overwrites_it() {
        let r = Provider::OpenRouter
            .request(
                "m",
                &json!({"tree": "1) Ajustes"}),
                &one_question(),
                &[screen()],
                false,
            )
            .expect("builds");
        assert_eq!(r.body["state"]["tree"], "1) Ajustes");
        assert_eq!(r.body["state"]["screen"]["type"], "image_url");

        let clash = Provider::OpenRouter
            .request(
                "m",
                &json!({"screen": "x"}),
                &one_question(),
                &[screen()],
                false,
            )
            .expect_err("refused");
        assert_eq!(clash.kind(), "image");

        let array = Provider::OpenRouter
            .request("m", &json!([1, 2]), &one_question(), &[screen()], false)
            .expect_err("refused");
        assert_eq!(array.code(), 5);
    }

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
