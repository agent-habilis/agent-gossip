pub(crate) fn directive_word(interior: &str) -> &str {
    interior.split_whitespace().next().unwrap_or("")
}
