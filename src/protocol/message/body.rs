//! [`MessageBody`] — a protocol message body (UTF-8 text). Newlines and
//! tabs are allowed (multi-line snippets); other control characters are
//! rejected. Empty is legal: presence and `PeerInfo` messages use it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A protocol message body — UTF-8 text. Newlines and tabs are allowed
/// (multi-line snippets); other control characters are rejected. Empty
/// is legal: presence and `PeerInfo` messages use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageBody(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyError(String);

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "message body must not contain control characters other than tab/newline, got {:?}",
            self.0
        )
    }
}

impl std::error::Error for BodyError {}

impl MessageBody {
    /// Construct a body. Accepts any UTF-8 text; the only restriction is
    /// control characters other than `\t`/`\n`/`\r`.
    ///
    /// # Errors
    /// Returns [`BodyError`] if `value` contains a disallowed control
    /// character (e.g. NUL or other C0/C1 controls).
    pub fn new(value: impl Into<String>) -> Result<Self, BodyError> {
        let value = value.into();
        if value
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t' | '\r'))
        {
            return Err(BodyError(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MessageBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageBody {
    type Err = BodyError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for MessageBody {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<[u8]> for MessageBody {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

#[cfg(test)]
impl From<&str> for MessageBody {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid message body in test fixture")
    }
}

#[cfg(test)]
mod body_tests {
    use super::MessageBody;

    #[test]
    fn new_accepts_ascii() {
        MessageBody::new("hello world").unwrap();
        MessageBody::new("").unwrap();
        MessageBody::new("special chars: !@#$%^&*()").unwrap();
    }

    #[test]
    fn new_accepts_unicode() {
        MessageBody::new("héllo").unwrap();
        MessageBody::new("emoji 🎉").unwrap();
        MessageBody::new("日本語のメッセージ").unwrap();
    }

    #[test]
    fn new_accepts_newline_and_tab() {
        MessageBody::new("line one\nline two").unwrap();
        MessageBody::new("col1\tcol2").unwrap();
        MessageBody::new("crlf\r\nline").unwrap();
    }

    #[test]
    fn new_rejects_control_chars() {
        assert!(MessageBody::new("nul\0byte").is_err());
        assert!(MessageBody::new("bell\u{7}char").is_err());
    }

    #[test]
    fn serde_transparent_round_trip() {
        let body = MessageBody::from("hello");
        let json = serde_json::to_string(&body).unwrap();
        assert_eq!(json, "\"hello\"");
        let parsed: MessageBody = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, body);
    }
}
