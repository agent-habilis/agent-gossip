/// Scan a value after its opening `"` up to the closing quote, honoring the
/// `\"`/`\\` escapes. Returns the value and the text after the closing quote.
pub(crate) fn parse_quoted_value<'text>(
    key: &str,
    mut value_text: &'text str,
) -> Result<(String, &'text str), String> {
    let mut value = String::new();
    loop {
        let Some(character) = value_text.chars().next() else {
            return Err(format!("unterminated value for `{key}`"));
        };
        value_text = &value_text[character.len_utf8()..];
        if character == '"' {
            return Ok((value, value_text));
        }
        if character != '\\' {
            value.push(character);
            continue;
        }
        let Some(escaped) = value_text.chars().next() else {
            return Err(format!("dangling escape in value for `{key}`"));
        };
        if escaped != '"' && escaped != '\\' {
            return Err(format!(
                "invalid escape `\\{escaped}` in value for `{key}` (only \\\" and \\\\)"
            ));
        }
        value.push(escaped);
        value_text = &value_text[escaped.len_utf8()..];
    }
}
