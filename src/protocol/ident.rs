//! Shared validation for human identifiers (`Nickname`, `SwarmName`).
//! "Safe UTF-8" means any scalar from any script (letters, marks,
//! numbers, symbols, emoji) except:
//!
//! - control characters and whitespace — unsafe to embed raw in the
//!   per-member filenames or in line-oriented output (logs, `--output json`);
//! - the Unicode `Bidi_Control` set (text-reordering Trojan-Source
//!   class, e.g. U+202E), which can disguise how a name renders;
//! - `<` `>` `#`, reserved for the prose display conventions `<nick>`
//!   and `#swarm`.
//!
//! The two types differ on the **path separators** `/` `\`. A `Nickname`
//! forbids them ([`is_forbidden`]) because it is embedded raw in the
//! per-member filenames (`transport::ipc` builds `<prefix>/<nick>.ipc.sock`,
//! and the log/state files mirror it), and clients recompute those paths — a
//! `/` would break the socket. A `SwarmName` is never in a path (paths key on
//! the base58 swarm-id prefix), so it allows `/ \` ([`is_forbidden_swarm_name`])
//! and may be a URL.
//!
//! This is not a full confusables/invisibles defense. Other
//! default-ignorable scalars such as U+200B ZWSP stay allowed, because
//! a blanket invisibles filter would also reject legitimate emoji
//! joiners (ZWJ/ZWNJ, U+200C/U+200D) and variation selectors.

/// Minimum identifier length in Unicode scalar values.
pub(super) const MIN_CHARS: usize = 1;

/// Maximum identifier length in Unicode scalar values.
pub(super) const MAX_CHARS: usize = 32;

/// Forbidden in a [`SwarmName`](crate::protocol::swarm::SwarmName): control,
/// whitespace, bidi-control, and the display-reserved `< > #`. Unlike a
/// nickname, a swarm name is never embedded in a filesystem path, so the path
/// separators `/ \` are allowed — a swarm name may be a URL.
pub(super) fn is_forbidden_swarm_name(ch: char) -> bool {
    ch.is_control() || ch.is_whitespace() || is_bidi_control(ch) || matches!(ch, '<' | '>' | '#')
}

/// Forbidden in a [`Nickname`](crate::protocol::Nickname): the swarm-name rule
/// plus the path separators `/ \`. A nickname is embedded raw in the per-member
/// filenames (`<prefix>/<nick>.ipc.sock`, `<nick>.tracing.log`,
/// `<nick>.state.json`), and clients recompute those paths to reach the daemon,
/// so a path separator would break the socket bind/connect.
pub(super) fn is_forbidden(ch: char) -> bool {
    is_forbidden_swarm_name(ch) || matches!(ch, '/' | '\\')
}

/// The Unicode `Bidi_Control` set: invisible scalars that reorder
/// surrounding text and can disguise how a name renders in a terminal
/// or filename. ZWJ/ZWNJ (U+200C/U+200D) are not in this set; they are
/// needed for emoji sequences and several scripts.
fn is_bidi_control(ch: char) -> bool {
    matches!(ch,
        '\u{061C}'                // ALM (Arabic Letter Mark)
        | '\u{200E}' | '\u{200F}' // LRM, RLM
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
    )
}
