pub(crate) fn is_name_char(character: char) -> bool {
    character.is_ascii_lowercase()
        || character.is_ascii_digit()
        || character == '_'
        || character == '-'
}
