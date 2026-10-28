use crate::is_name_char::is_name_char;
use crate::parse_quoted_value::parse_quoted_value;

/// Parse the `key="value" ...` tail of a directive. All inputs are named;
/// values are double-quoted with `\"` and `\\` as the only escapes. Bare
/// tokens, duplicate keys, bad escapes, and unterminated values are errors.
pub(crate) fn parse_named_args(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let key_len = rest.chars().take_while(|ch| is_name_char(*ch)).count();
        if key_len == 0 {
            return Err(format!("expected key=\"value\", found {rest:?}"));
        }
        let key = &rest[..key_len];
        rest = &rest[key_len..];
        let Some(after_eq) = rest.strip_prefix('=') else {
            return Err(format!("key `{key}` must be followed by =\"value\""));
        };
        let Some(quoted) = after_eq.strip_prefix('"') else {
            return Err(format!("value for `{key}` must be double-quoted"));
        };
        let (value, value_text) = parse_quoted_value(key, quoted)?;
        if pairs.iter().any(|(existing, _)| existing == key) {
            return Err(format!("duplicate key `{key}`"));
        }
        pairs.push((key.to_owned(), value));
        let trimmed = value_text.trim_start();
        if trimmed.len() == value_text.len() && !trimmed.is_empty() {
            return Err(format!("missing whitespace after value for `{key}`"));
        }
        rest = trimmed;
    }
    Ok(pairs)
}
