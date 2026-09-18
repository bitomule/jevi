use std::fmt;

/// Two failure classes, and the difference is whether retrying could ever help.
///
/// `NoAnswer` is infrastructure: no key, no network, the API is down. Nothing about the
/// request was wrong, so a caller that falls back to its previous behaviour is correct.
///
/// `Invalid` is the request itself: a malformed question file, an empty state, a body the
/// API rejected. Retrying reproduces it exactly.
#[derive(Debug)]
pub enum Error {
    NoAnswer { kind: &'static str, message: String },
    Invalid { kind: &'static str, message: String },
}

impl Error {
    pub fn no_answer(kind: &'static str, message: impl Into<String>) -> Self {
        Error::NoAnswer {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(kind: &'static str, message: impl Into<String>) -> Self {
        Error::Invalid {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Error::NoAnswer { kind, .. } | Error::Invalid { kind, .. } => kind,
        }
    }

    /// 4 for "we could not get an answer", 5 for "this request was never going to work".
    /// Never 2: Claude Code reads a hook's exit 2 as "block this tool call and hand my
    /// stderr to the model", so a typo in a hook's flags must not become a block.
    pub fn code(&self) -> i32 {
        match self {
            Error::NoAnswer { .. } => 4,
            Error::Invalid { .. } => 5,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoAnswer { kind, message } => write!(f, "no answer ({kind}: {message})"),
            Error::Invalid { kind, message } => write!(f, "invalid input ({kind}: {message})"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
