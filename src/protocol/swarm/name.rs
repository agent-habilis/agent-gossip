//! [`SwarmName`] — a human-readable swarm label, bound cryptographically
//! into the topic id. Same charset rules as `Nickname` (see
//! [`crate::protocol::ident`]); the newtype is the single validation point.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use crate::protocol::{ident, wordlist};

/// A human-readable swarm label, bound cryptographically into the topic id.
///
/// Same rules as `Nickname`: 1..=32 "safe UTF-8" scalar values from any
/// script; see `crate::protocol::ident` for the exact exclusions (control,
/// whitespace, path separators, bidi formatting, and `<` `>` `#`
/// reserved for the `<nick>`/`#swarm` display conventions). The newtype
/// is the single validation point — every construction path goes
/// through `new`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SwarmName(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Length(usize),
    Charset(String),
}

impl fmt::Display for NameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::Length(len) => {
                write!(
                    formatter,
                    "swarm name must be {}..={} characters, got {len}",
                    ident::MIN_CHARS,
                    ident::MAX_CHARS
                )
            }
            NameError::Charset(value) => {
                write!(
                    formatter,
                    "swarm name must not contain control characters, whitespace, bidirectional formatting characters, or any of / \\ < > #, got {value:?}"
                )
            }
        }
    }
}

impl std::error::Error for NameError {}

impl SwarmName {
    /// Validate and wrap a swarm name. The single construction path —
    /// every `SwarmName` is guaranteed to satisfy the charset/length rules.
    ///
    /// # Errors
    /// Returns [`NameError`] if `value` is empty, longer than 32 scalar
    /// values, or contains a forbidden character (control, whitespace,
    /// path separator, bidi formatting, or any of `/ \ < > #`).
    pub fn new(value: impl Into<String>) -> Result<Self, NameError> {
        let value = value.into();
        let count = value.chars().count();
        if !(ident::MIN_CHARS..=ident::MAX_CHARS).contains(&count) {
            return Err(NameError::Length(count));
        }
        if value.chars().any(ident::is_forbidden) {
            return Err(NameError::Charset(value));
        }
        Ok(Self(value))
    }

    /// Generate a random `word-word` swarm name from the curated
    /// wordlist — the same generator nicknames use.
    ///
    /// # Panics
    /// Never in practice: every wordlist pair is a lowercase-ASCII
    /// constant, so selection and validation always succeed.
    #[must_use]
    pub fn random() -> Self {
        Self::new(wordlist::random_pair()).expect("wordlist pair is always a valid swarm name")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Byte length as a `u8`. `new` bounds the name to `MAX_CHARS`
    /// scalar values (<= `NAME_MAX_BYTES` = 128 bytes), so this never
    /// truncates.
    pub(crate) fn len_u8(&self) -> u8 {
        u8::try_from(self.0.len()).expect("SwarmName is <= 128 bytes")
    }
}

impl fmt::Display for SwarmName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SwarmName {
    type Err = NameError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}
