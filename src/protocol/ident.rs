//! Shared validation for human identifiers (`Nickname`, `SwarmName`).
//! "Safe UTF-8" means any scalar from any script (letters, marks,
//! numbers, symbols, emoji) except:
//!
//! - control characters, whitespace, and the path separators `/` `\` —
//!   unsafe to embed raw in socket/log filenames (`transport::ipc`
//!   builds `<prefix>-<nick>.sock`/`.log`) or in line-oriented output
//!   (logs, `--output json`);
//! - the Unicode `Bidi_Control` set (text-reordering Trojan-Source
//!   class, e.g. U+202E), which can disguise how a name renders;
//! - `<` `>` `#`, reserved for the prose display conventions `<nick>`
//!   and `#swarm`. `#` only marks swarms, but both types share this one
//!   rule, so it is reserved in both.
//!
//! This is not a full confusables/invisibles defense. Other
//! default-ignorable scalars such as U+200B ZWSP stay allowed, because
//! a blanket invisibles filter would also reject legitimate emoji
//! joiners (ZWJ/ZWNJ, U+200C/U+200D) and variation selectors.

/// Minimum identifier length in Unicode scalar values.
pub(super) const MIN_CHARS: usize = 1;

/// Maximum identifier length in Unicode scalar values.
pub(super) const MAX_CHARS: usize = 32;

/// Whether `ch` is disallowed in an identifier.
pub(super) fn is_forbidden(ch: char) -> bool {
    ch.is_control()
        || ch.is_whitespace()
        || matches!(ch, '/' | '\\' | '<' | '>' | '#')
        || is_bidi_control(ch)
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
