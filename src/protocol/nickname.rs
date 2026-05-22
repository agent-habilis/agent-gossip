use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

const NICKNAME_MAX_LEN: usize = 32;

/// An agent nickname — lowercase ASCII identifier.
///
/// Charset `[a-z0-9_-]`, must start with a lowercase letter, length
/// 1..=32. The wordlist generator (`Nickname::random`) emits
/// `word-word` pairs that fit comfortably inside this range; the
/// angle-bracket display format (e.g. `<swift-cedar>`) relies on the
/// charset excluding `<` and `>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nickname(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NicknameError {
    Length(usize),
    Charset(String),
    LeadingChar(char),
}

impl fmt::Display for NicknameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NicknameError::Length(len) => {
                write!(
                    formatter,
                    "nickname must be 1..={NICKNAME_MAX_LEN} chars, got {len}"
                )
            }
            NicknameError::LeadingChar(ch) => {
                write!(
                    formatter,
                    "nickname must start with a lowercase letter, got {ch:?}"
                )
            }
            NicknameError::Charset(value) => {
                write!(
                    formatter,
                    "nickname must contain only [a-z0-9_-], got {value:?}"
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
    /// Returns [`NicknameError`] if `value` is empty or longer than 32
    /// chars, does not start with a lowercase ASCII letter, or contains
    /// a character outside `[a-z0-9_-]`.
    pub fn new(value: impl Into<String>) -> Result<Self, NicknameError> {
        let value = value.into();
        if value.is_empty() || value.len() > NICKNAME_MAX_LEN {
            return Err(NicknameError::Length(value.len()));
        }
        let Some(first) = value.chars().next() else {
            return Err(NicknameError::Length(value.len()));
        };
        if !first.is_ascii_lowercase() {
            return Err(NicknameError::LeadingChar(first));
        }
        if !value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
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
        // Wordlist is curated lowercase ASCII, so this always validates.
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
    use super::*;

    #[test]
    fn new_accepts_valid_nicknames() {
        for ok in [
            "a",
            "alice",
            "swift-cedar",
            "alice-bot-7",
            "snake_case",
            "abc123",
        ] {
            Nickname::new(ok).unwrap_or_else(|_| panic!("expected {ok} to validate"));
        }
    }

    #[test]
    fn new_rejects_uppercase() {
        assert!(Nickname::new("Alice").is_err());
        assert!(Nickname::new("aLice").is_err());
    }

    #[test]
    fn new_rejects_empty_and_overlong() {
        assert!(Nickname::new("").is_err());
        let too_long = "a".repeat(NICKNAME_MAX_LEN + 1);
        assert!(Nickname::new(too_long).is_err());
    }

    #[test]
    fn new_rejects_leading_digit_or_dash() {
        assert!(Nickname::new("1abc").is_err());
        assert!(Nickname::new("-abc").is_err());
    }

    #[test]
    fn new_rejects_invalid_charset() {
        assert!(Nickname::new("alice bot").is_err());
        assert!(Nickname::new("alice!").is_err());
        assert!(Nickname::new("alice/bob").is_err());
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
