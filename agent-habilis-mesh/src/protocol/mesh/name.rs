//! [`MeshName`] — a human-readable mesh label, bound cryptographically
//! into the topic id. Nearly the same charset as `Nickname` (see
//! [`crate::protocol::ident`]) but, unlike a nickname, a mesh name may contain
//! the path separators `/ \` (it is never a filename), so it can be a URL. The
//! newtype is the single validation point.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use crate::protocol::{ident, wordlist};

/// A human-readable mesh label, bound cryptographically into the topic id.
///
/// 1..=32 "safe UTF-8" scalar values from any script; see
/// `crate::protocol::ident` for the exact exclusions (control, whitespace,
/// bidi formatting, and `<` `>` `#` reserved for the `<nick>`/`#mesh` display
/// conventions). Unlike a `Nickname`, a mesh name **may contain the path
/// separators `/ \`** — it is never used in a filesystem path — so a mesh name
/// can be a URL. The newtype is the single validation point — every
/// construction path goes through `new`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MeshName(String);

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
                    "mesh name must be {}..={} characters, got {len}",
                    ident::MIN_CHARS,
                    ident::MAX_CHARS
                )
            }
            NameError::Charset(value) => {
                write!(
                    formatter,
                    "mesh name must not contain control characters, whitespace, bidirectional formatting characters, or any of < > #, got {value:?}"
                )
            }
        }
    }
}

impl std::error::Error for NameError {}

/// Whether `input` starts with the `http://` or `https://` scheme
/// (case-insensitive, per RFC 3986 — schemes are case-insensitive).
fn is_http_url(input: &str) -> bool {
    input
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || input
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

/// Reduce a topic string to the source for its display name: drop a leading URL
/// scheme, and — for an **http(s)** URL — also drop the `?query` and `#fragment`
/// so the name is just host + path. Display-only; the seed still hashes the
/// full string.
fn topic_name_source(trimmed: &str) -> &str {
    let source = strip_url_scheme(trimmed);
    if is_http_url(trimmed) {
        let end = source.find(['?', '#']).unwrap_or(source.len());
        &source[..end]
    } else {
        source
    }
}

/// Drop a leading URL scheme (`https://`, `git+ssh://`, …) from a topic string
/// so the derived name reads cleanly. Only the `<scheme>://` form is stripped,
/// where `scheme` is `[A-Za-z][A-Za-z0-9+.-]*`; anything else (e.g. `note:x`,
/// or a `://` with a non-scheme prefix) is returned unchanged.
fn strip_url_scheme(input: &str) -> &str {
    if let Some(idx) = input.find("://") {
        let scheme = &input[..idx];
        let mut chars = scheme.chars();
        let valid = chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '.' | '-'));
        if valid {
            return &input[idx + 3..];
        }
    }
    input
}

impl MeshName {
    /// Validate and wrap a mesh name. The single construction path —
    /// every `MeshName` is guaranteed to satisfy the charset/length rules.
    ///
    /// # Errors
    /// Returns [`NameError`] if `value` is empty, longer than 32 scalar
    /// values, or contains a forbidden character (control, whitespace, bidi
    /// formatting, or any of `< > #`).
    pub fn new(value: impl Into<String>) -> Result<Self, NameError> {
        let value = value.into();
        let count = value.chars().count();
        if !(ident::MIN_CHARS..=ident::MAX_CHARS).contains(&count) {
            return Err(NameError::Length(count));
        }
        if value.chars().any(ident::is_forbidden_mesh_name) {
            return Err(NameError::Charset(value));
        }
        Ok(Self(value))
    }

    /// Generate a random `word-word` mesh name from the curated
    /// wordlist — the same generator nicknames use.
    ///
    /// # Panics
    /// Never in practice: every wordlist pair is a lowercase-ASCII
    /// constant, so selection and validation always succeed.
    #[must_use]
    pub fn random() -> Self {
        Self::new(wordlist::random_pair()).expect("wordlist pair is always a valid mesh name")
    }

    /// Derive a `topic` mesh's display name from its shared string. A leading
    /// URL scheme (`https://`, `git://`, …) is dropped so the name reads
    /// cleanly, and for an http(s) URL the `?query`/`#fragment` are dropped too
    /// (display only — the seed still hashes the full string); then every run of
    /// invalid chars ([`ident::is_forbidden_mesh_name`] —
    /// whitespace, control, bidi, `< > #`) becomes a single `-`
    /// (leading/trailing suppressed), capped at [`ident::MAX_CHARS`] scalar
    /// values (an over-long name keeps the first `MAX_CHARS - 1` and ends in
    /// `…`), falling back to `topic` when nothing valid survives. Path
    /// separators `/ \` and the rest of the URL charset survive verbatim.
    /// Deterministic, so every peer passing the same string derives the same
    /// name.
    ///
    /// # Panics
    /// Never: the sanitized string (or the `topic` fallback) is always a valid
    /// name — every kept char is non-forbidden, `-` and `…` are allowed, and
    /// the length is bounded.
    #[must_use]
    pub(crate) fn from_topic_string(raw: &str) -> Self {
        let mut out = String::new();
        let mut pending_dash = false;
        for ch in topic_name_source(raw.trim()).chars() {
            if ident::is_forbidden_mesh_name(ch) {
                // Any invalid char (incl. whitespace) → a single `-`; runs
                // collapse and a leading `-` is suppressed. A trailing run
                // leaves `pending_dash` set but is never emitted.
                pending_dash = !out.is_empty();
                continue;
            }
            if pending_dash {
                out.push('-');
                pending_dash = false;
            }
            out.push(ch);
        }

        let chars: Vec<char> = out.chars().collect();
        let name = if chars.len() <= ident::MAX_CHARS {
            out
        } else {
            // Keep MAX_CHARS-1 scalar values, drop a trailing `-` a mid-run cut
            // may leave, then mark the truncation with `…` (total <= MAX_CHARS).
            let mut head: String = chars[..ident::MAX_CHARS - 1].iter().collect();
            while head.ends_with('-') {
                head.pop();
            }
            head.push('…');
            head
        };

        Self::new(name).unwrap_or_else(|_| Self::new("topic").expect("`topic` is a valid name"))
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
        u8::try_from(self.0.len()).expect("MeshName is <= 128 bytes")
    }
}

impl fmt::Display for MeshName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MeshName {
    type Err = NameError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::{MeshName, ident};

    #[test]
    fn from_topic_string_passthrough_and_case_preserving() {
        assert_eq!(
            MeshName::from_topic_string("agent-habilis").as_str(),
            "agent-habilis"
        );
        assert_eq!(MeshName::from_topic_string("Repo").as_str(), "Repo");
    }

    #[test]
    fn url_scheme_stripped_and_slashes_kept() {
        // The scheme is stripped and the `/`s kept; this URL also exceeds the
        // 32-char name cap, so the tail truncates to `…` (see `MAX_CHARS`).
        assert_eq!(
            MeshName::from_topic_string("https://github.com/agent-habilis/agent-square").as_str(),
            "github.com/agent-habilis/agent…"
        );
        assert_eq!(MeshName::from_topic_string("git://h/a").as_str(), "h/a");
        // Only the `scheme://` form is stripped.
        assert_eq!(MeshName::from_topic_string("note:x").as_str(), "note:x");
    }

    #[test]
    fn http_url_drops_query_and_fragment() {
        assert_eq!(
            MeshName::from_topic_string(
                "https://github.com/agent-habilis/agent-square?tab=readme#install"
            )
            .as_str(),
            "github.com/agent-habilis/agent…"
        );
        assert_eq!(
            MeshName::from_topic_string("https://x/p?q=1#f").as_str(),
            "x/p"
        );
        assert_eq!(MeshName::from_topic_string("https://x#f").as_str(), "x");
        // Scheme detection is case-insensitive.
        assert_eq!(MeshName::from_topic_string("HTTP://x?y").as_str(), "x");
        // Non-http schemes keep the query (scheme-strip only).
        assert_eq!(MeshName::from_topic_string("git://x?y").as_str(), "x?y");
        // Empty after stripping the query ⇒ the `topic` fallback.
        assert_eq!(MeshName::from_topic_string("https://?q").as_str(), "topic");
    }

    #[test]
    fn forbidden_runs_collapse_to_single_dash() {
        // whitespace, `#`, `<>`, and control collapse to one `-`; leading and
        // trailing suppressed. `/` is NOT forbidden and survives verbatim.
        assert_eq!(
            MeshName::from_topic_string("my secret room").as_str(),
            "my-secret-room"
        );
        assert_eq!(MeshName::from_topic_string("a#b").as_str(), "a-b");
        assert_eq!(MeshName::from_topic_string("a\u{0007}b").as_str(), "a-b");
        assert_eq!(MeshName::from_topic_string("  a/b  ").as_str(), "a/b");
    }

    #[test]
    fn overlong_name_truncates_with_ellipsis() {
        let name = MeshName::from_topic_string(&"x".repeat(100));
        assert_eq!(name.as_str().chars().count(), ident::MAX_CHARS);
        assert!(name.as_str().ends_with('…'), "got {name}");
        // The `…` never lands right after a truncation dash.
        assert!(!name.as_str().ends_with("-…"), "got {name}");
    }

    #[test]
    fn empty_after_filtering_falls_back_to_topic() {
        assert_eq!(MeshName::from_topic_string("###  <>").as_str(), "topic");
        assert_eq!(MeshName::from_topic_string("https://").as_str(), "topic");
    }

    #[test]
    fn all_url_rfc_chars_except_hash_are_valid_names() {
        // RFC 3986 sub/gen-delims + unreserved punctuation, minus `#`.
        // (Letters/digits are obviously valid.)
        let url_specials = "-._~:/?[]@!$&'()*+,;=%";
        assert!(
            MeshName::new(url_specials).is_ok(),
            "the URL punctuation set must be a valid mesh name"
        );
        for ch in url_specials.chars() {
            assert!(
                !ident::is_forbidden_mesh_name(ch),
                "URL char {ch:?} must be allowed in a mesh name"
            );
        }
        // The reserved trio stays forbidden.
        for ch in ['#', '<', '>'] {
            assert!(
                ident::is_forbidden_mesh_name(ch),
                "{ch:?} must stay forbidden in a mesh name"
            );
        }
    }
}
