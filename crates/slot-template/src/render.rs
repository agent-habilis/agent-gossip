use crate::directive_word::directive_word;
use crate::error::Error;
use crate::parse_named_args::parse_named_args;

/// Substitute every `<!-- slot name="..." -->` in `template` from `args`,
/// purely: same (template, args) always yields the same bytes. Any other HTML
/// comment — and every non-comment byte — passes through untouched. A comment
/// that starts as a directive but fails the grammar is an error, so a typo'd
/// directive cannot slip through unrendered; an `include` directive here (not
/// alone on its own line — [`expand`](fn@crate::expand) consumes those) is likewise an error.
///
/// # Errors
/// [`Error::UnknownSlot`] for a slot with no matching arg,
/// [`Error::UnusedArg`] for an arg with no matching slot,
/// [`Error::MalformedSlot`] / [`Error::MalformedInclude`] for a directive
/// that fails the grammar.
pub(crate) fn render(template: &str, args: &[(&str, &str)]) -> Result<String, Error> {
    let mut output = String::with_capacity(template.len());
    let mut used = vec![false; args.len()];
    let mut cursor = 0;
    while let Some(open) = template[cursor..].find("<!--").map(|found| cursor + found) {
        output.push_str(&template[cursor..open]);
        let after_open = &template[open + 4..];
        let Some(close) = after_open.find("-->") else {
            match directive_word(after_open) {
                "slot" | "include" => {
                    return Err(Error::MalformedSlot {
                        offset: open,
                        reason: "unterminated directive comment".to_owned(),
                    });
                }
                _ => {
                    output.push_str(&template[open..]);
                    cursor = template.len();
                    break;
                }
            }
        };
        let interior = &after_open[..close];
        let end = open + 4 + close + 3;
        match directive_word(interior) {
            "slot" => {
                let tail = interior.trim_start().trim_start_matches("slot");
                let malformed = |reason: String| Error::MalformedSlot {
                    offset: open,
                    reason,
                };
                let pairs = parse_named_args(tail).map_err(malformed)?;
                let [(key, name)] = pairs.as_slice() else {
                    return Err(malformed("slot takes exactly name=\"...\"".to_owned()));
                };
                if key != "name" {
                    return Err(malformed(format!("slot takes name=\"...\", not `{key}`")));
                }
                let position = args
                    .iter()
                    .position(|(arg, _)| arg == name)
                    .ok_or_else(|| Error::UnknownSlot { name: name.clone() })?;
                used[position] = true;
                output.push_str(args[position].1);
            }
            "include" => {
                return Err(Error::MalformedInclude {
                    line: template[..open].lines().count().max(1),
                    reason: "include must be alone on its own line".to_owned(),
                });
            }
            _ => output.push_str(&template[open..end]),
        }
        cursor = end;
    }
    output.push_str(&template[cursor..]);
    if let Some(position) = used.iter().position(|was_used| !was_used) {
        return Err(Error::UnusedArg {
            name: args[position].0.to_owned(),
        });
    }
    Ok(output)
}
