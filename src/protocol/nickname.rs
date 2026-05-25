use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An agent nickname — a "safe UTF-8" identifier.
///
/// 1..=32 Unicode scalar values from any script (letters, marks,
/// numbers, symbols, emoji); see `super::ident` for the exact
/// exclusions (control, whitespace, path separators, bidi formatting,
/// and `<` `>` `#` reserved for the `<nick>`/`#swarm` display forms).
/// The wordlist generator (`Nickname::random`) emits `word-word` pairs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nickname(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NicknameError {
    Length(usize),
    Charset(String),
}

impl fmt::Display for NicknameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NicknameError::Length(len) => {
                write!(
                    formatter,
                    "nickname must be {}..={} characters, got {len}",
                    super::ident::MIN_CHARS,
                    super::ident::MAX_CHARS
                )
            }
            NicknameError::Charset(value) => {
                write!(
                    formatter,
                    "nickname must not contain control characters, whitespace, bidirectional formatting characters, or any of / \\ < > #, got {value:?}"
                )
            }
        }
    }
}

impl std::error::Error for NicknameError {}

impl Nickname {
    /// Validate and construct a nickname.
    ///
    /// # Errors
    /// Returns [`NicknameError`] if `value` is empty, longer than 32
    /// characters, or contains a forbidden scalar (see
    /// `is_forbidden`).
    pub fn new(value: impl Into<String>) -> Result<Self, NicknameError> {
        let value = value.into();
        let count = value.chars().count();
        if !(super::ident::MIN_CHARS..=super::ident::MAX_CHARS).contains(&count) {
            return Err(NicknameError::Length(count));
        }
        if value.chars().any(super::ident::is_forbidden) {
            return Err(NicknameError::Charset(value));
        }
        Ok(Self(value))
    }

    /// Generate a random `word-word` nickname from the curated wordlist.
    ///
    /// # Panics
    /// Never in practice: the curated wordlist is a non-empty,
    /// lowercase-ASCII constant, so selection and validation always
    /// succeed.
    #[must_use]
    pub fn random() -> Self {
        Self::new(super::wordlist::random_pair())
            .expect("wordlist combinations are always valid nicknames")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Nickname {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for Nickname {
    type Err = NicknameError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl AsRef<str> for Nickname {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Nickname {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
impl From<&str> for Nickname {
    fn from(text: &str) -> Self {
        Self::new(text).expect("invalid nickname in test fixture")
    }
}

#[cfg(test)]
mod tests {
    use super::Nickname;

    #[test]
    fn new_accepts_valid_nicknames() {
        for ok in [
            "a",
            "alice",
            "swift-cedar",
            "alice-bot-7",
            "snake_case",
            "abc123",
            "Alice",    // uppercase
            "1abc",     // leading digit
            "café",     // accents
            "日本語",   // CJK
            "agent-🐝", // emoji
            "👨‍👩‍👧",       // ZWJ emoji sequence
            "alice!",   // punctuation symbol
        ] {
            Nickname::new(ok).unwrap_or_else(|_| panic!("expected {ok} to validate"));
        }
    }

    #[test]
    fn new_rejects_empty_and_overlong() {
        assert!(Nickname::new("").is_err());
        let too_long = "a".repeat(super::super::ident::MAX_CHARS + 1);
        assert!(Nickname::new(too_long).is_err());
    }

    #[test]
    fn length_cap_counts_characters_not_bytes() {
        // 32 multibyte chars (96 bytes) is fine; 33 is not.
        assert!(Nickname::new("あ".repeat(32)).is_ok());
        assert!(Nickname::new("あ".repeat(33)).is_err());
    }

    #[test]
    fn new_rejects_unsafe_chars() {
        assert!(Nickname::new("alice bot").is_err()); // space
        assert!(Nickname::new("alice/bob").is_err()); // path sep
        assert!(Nickname::new("alice\\bob").is_err()); // path sep
        assert!(Nickname::new("alice\nbob").is_err()); // newline
        assert!(Nickname::new("alice\0bob").is_err()); // NUL
        assert!(Nickname::new("alice\u{7}bob").is_err()); // bell control
        assert!(Nickname::new("alice\u{202E}bob").is_err()); // RLO (bidi override)
        assert!(Nickname::new("alice\u{200F}bob").is_err()); // RLM (bidi mark)
        assert!(Nickname::new("alice\u{61C}bob").is_err()); // ALM (Arabic letter mark)
        assert!(Nickname::new("a<b").is_err()); // reserved for <nick>
        assert!(Nickname::new("a>b").is_err()); // reserved for <nick>
        assert!(Nickname::new("a#b").is_err()); // reserved for #swarm
    }

    #[test]
    fn random_always_validates() {
        for _ in 0..20 {
            let nickname = Nickname::random();
            Nickname::new(nickname.as_str()).expect("random must round-trip");
        }
    }

    #[test]
    fn serde_transparent_round_trip() {
        let nickname = Nickname::from("swift-cedar");
        let json = serde_json::to_string(&nickname).unwrap();
        assert_eq!(json, "\"swift-cedar\"");
        let parsed: Nickname = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, nickname);
    }
}
