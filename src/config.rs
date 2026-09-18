//! Where settings and credentials come from, and in what order.
//!
//! Never a `.env` file. Claude Code ships a hook that denies reading any `.env*` path, so a
//! key kept there is a key the tooling around this cannot see. The convention here is the
//! one the surrounding skills already use: environment variable first, then
//! `$XDG_CONFIG_HOME/bitomule/jevi/config.json` at mode 0600.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::provider::Provider;

pub struct Config {
    root: Value,
    pub loaded: bool,
}

pub fn config_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("bitomule/jevi");
        }
    }
    home().join(".config/bitomule/jevi")
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    match std::env::var("JEVI_CONFIG") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => config_dir().join("config.json"),
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let root = serde_json::from_str(&raw)
                    .map_err(|e| Error::invalid("config", format!("{}: {e}", path.display())))?;
                Ok(Config { root, loaded: true })
            }
            Err(_) => Ok(Config {
                root: Value::Null,
                loaded: false,
            }),
        }
    }

    fn top(&self, key: &str) -> Option<&Value> {
        self.root.get(key)
    }

    fn per_provider(&self, p: Provider, key: &str) -> Option<&str> {
        self.root
            .get(p.name())?
            .get(key)?
            .as_str()
            .filter(|s| !s.is_empty())
    }

    /// Whichever provider has a credential. With both, OpenRouter — it is the shape this
    /// build has actually verified. Flip this the day the direct endpoint is confirmed.
    pub fn provider(&self, flag: Option<&str>) -> Result<Provider> {
        if let Some(name) = flag {
            return Provider::parse(name);
        }
        if let Ok(name) = std::env::var("JEVI_PROVIDER") {
            if !name.is_empty() {
                return Provider::parse(&name);
            }
        }
        if let Some(name) = self.top("provider").and_then(Value::as_str) {
            return Provider::parse(name);
        }
        for p in [Provider::OpenRouter, Provider::TypeSafe] {
            if self.key_for(p).is_some() {
                return Ok(p);
            }
        }
        Ok(Provider::OpenRouter)
    }

    pub fn key_for(&self, p: Provider) -> Option<String> {
        if let Ok(k) = std::env::var("JEVI_API_KEY") {
            if !k.is_empty() {
                return Some(k);
            }
        }
        if let Ok(k) = std::env::var(p.env_key()) {
            if !k.is_empty() {
                return Some(k);
            }
        }
        self.per_provider(p, "api_key").map(str::to_owned)
    }

    pub fn model(&self, p: Provider, flag: Option<&str>) -> String {
        flag.map(str::to_owned)
            .or_else(|| std::env::var("JEVI_MODEL").ok().filter(|s| !s.is_empty()))
            .or_else(|| self.per_provider(p, "model").map(str::to_owned))
            .unwrap_or_else(|| p.default_model().to_owned())
    }

    pub fn url(&self, p: Provider) -> String {
        self.per_provider(p, "base_url")
            .map(str::to_owned)
            .unwrap_or_else(|| p.default_url().to_owned())
    }

    pub fn timeout_ms(&self, flag: Option<u64>) -> u64 {
        flag.or_else(|| std::env::var("JEVI_TIMEOUT_MS").ok()?.parse().ok())
            .or_else(|| self.top("timeout_ms")?.as_u64())
            .unwrap_or(3000)
    }

    /// Question sets are looked up project-local first, so a repo can carry its own
    /// thresholds, then globally.
    pub fn resolve_set(&self, name: &str) -> Result<(String, String)> {
        let direct = Path::new(name);
        if direct.exists() {
            let raw = std::fs::read_to_string(direct)
                .map_err(|e| Error::invalid("question_file", format!("{name}: {e}")))?;
            return Ok((raw, name.to_owned()));
        }
        if name.contains('/') {
            return Err(Error::invalid(
                "question_file",
                format!("{name}: no such file"),
            ));
        }
        let candidates = [
            PathBuf::from(".jevi").join(format!("{name}.json")),
            config_dir().join("questions").join(format!("{name}.json")),
        ];
        for c in &candidates {
            if let Ok(raw) = std::fs::read_to_string(c) {
                return Ok((raw, c.display().to_string()));
            }
        }
        Err(Error::invalid(
            "question_file",
            format!(
                "no question set `{name}` (looked in ./.jevi/ and {})",
                config_dir().join("questions").display()
            ),
        ))
    }
}
