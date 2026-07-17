//! Pure-function markdown templating over two HTML-comment directives, so a
//! template source stays valid, cleanly-rendering markdown:
//!
//! - `<!-- include path="..." key="value" ... -->` — alone on its own line:
//!   splice another file, resolved relative to the file containing the line,
//!   rendered with EXACTLY the args named at that call site.
//! - `<!-- slot name="..." -->` — anywhere: the substitution point an include
//!   arg fills.
//!
//! No engine, no ambient context, no escaping of substituted values. Strict
//! in both directions — a slot without an arg and an arg without a slot are
//! both errors — so call sites are checked like function signatures. A
//! comment that starts as a directive but fails the grammar is an error,
//! never silently literal; every other HTML comment and every other byte
//! passes through byte-exact.

use std::fmt;
use std::path::{Component, Path, PathBuf};

const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The template names a slot the call site did not provide.
    UnknownSlot { name: String },
    /// The call site passed an arg no `<!-- slot -->` in the template consumes.
    UnusedArg { name: String },
    /// A comment starting `<!-- slot` that fails the directive grammar.
    MalformedSlot { offset: usize, reason: String },
    /// A comment starting `<!-- include` that fails the directive grammar
    /// (or is not alone on its own line).
    MalformedInclude { line: usize, reason: String },
    /// An include chain revisits a file.
    IncludeCycle { path: PathBuf },
    /// An include chain nests deeper than [`MAX_DEPTH`].
    DepthExceeded { path: PathBuf },
    /// The loader could not produce the file.
    Load { path: PathBuf, reason: String },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnknownSlot { name } => {
                write!(formatter, "slot {name:?} has no arg providing it")
            }
            Error::UnusedArg { name } => {
                write!(formatter, "arg {name:?} matches no slot in the template")
            }
            Error::MalformedSlot { offset, reason } => {
                write!(
                    formatter,
                    "malformed slot directive at byte {offset}: {reason}"
                )
            }
            Error::MalformedInclude { line, reason } => {
                write!(
                    formatter,
                    "malformed include directive on line {line}: {reason}"
                )
            }
            Error::IncludeCycle { path } => {
                write!(formatter, "include cycle through {}", path.display())
            }
            Error::DepthExceeded { path } => {
                write!(
                    formatter,
                    "include depth exceeds {MAX_DEPTH} at {}",
                    path.display()
                )
            }
            Error::Load { path, reason } => {
                write!(formatter, "cannot load {}: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {}

fn is_name_char(character: char) -> bool {
    character.is_ascii_lowercase()
        || character.is_ascii_digit()
        || character == '_'
        || character == '-'
}

/// Parse the `key="value" ...` tail of a directive. All inputs are named;
/// values are double-quoted with `\"` and `\\` as the only escapes. Bare
/// tokens, duplicate keys, bad escapes, and unterminated values are errors.
fn parse_named_args(text: &str) -> Result<Vec<(String, String)>, String> {
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

/// Scan a value after its opening `"` up to the closing quote, honoring the
/// `\"`/`\\` escapes. Returns the value and the text after the closing quote.
fn parse_quoted_value<'text>(
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

fn directive_word(interior: &str) -> &str {
    interior.split_whitespace().next().unwrap_or("")
}

/// Substitute every `<!-- slot name="..." -->` in `template` from `args`,
/// purely: same (template, args) always yields the same bytes. Any other HTML
/// comment — and every non-comment byte — passes through untouched. A comment
/// that starts as a directive but fails the grammar is an error, so a typo'd
/// directive cannot slip through unrendered; an `include` directive here (not
/// alone on its own line — [`expand`] consumes those) is likewise an error.
///
/// # Errors
/// [`Error::UnknownSlot`] for a slot with no matching arg,
/// [`Error::UnusedArg`] for an arg with no matching slot,
/// [`Error::MalformedSlot`] / [`Error::MalformedInclude`] for a directive
/// that fails the grammar.
pub fn render(template: &str, args: &[(&str, &str)]) -> Result<String, Error> {
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

/// Lexical path normalization (`a/../b` → `b`) so cycle detection and the
/// loader see one canonical spelling per file, without touching the fs.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // `..` may only cancel a *named* component. Popping blindly makes a
            // leading `..` cancel the one before it (`../../x` → `x`), and root
            // has no parent to climb to (`/..` → `/`).
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir) => {}
                _ => normalized.push(".."),
            },
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component);
            }
        }
    }
    normalized
}

/// Load `path` through `loader` and render it: own-line
/// `<!-- include path="..." ... -->` lines splice the named file (resolved
/// relative to the including file, rendered with exactly the args at that
/// call site), and `<!-- slot -->` directives substitute from `args`. Spliced
/// content is final — it is never re-substituted by the includer.
///
/// # Errors
/// Everything [`render`] returns, plus [`Error::MalformedInclude`] for a bad
/// include line, [`Error::IncludeCycle`] / [`Error::DepthExceeded`] for
/// degenerate include graphs, and [`Error::Load`] when `loader` fails.
pub fn expand(
    path: &Path,
    args: &[(&str, &str)],
    loader: &mut dyn FnMut(&Path) -> Result<String, String>,
) -> Result<String, Error> {
    expand_at(path, args, loader, &mut Vec::new())
}

fn expand_at(
    path: &Path,
    args: &[(&str, &str)],
    loader: &mut dyn FnMut(&Path) -> Result<String, String>,
    stack: &mut Vec<PathBuf>,
) -> Result<String, Error> {
    let path = normalize(path);
    if stack.contains(&path) {
        return Err(Error::IncludeCycle { path });
    }
    if stack.len() >= MAX_DEPTH {
        return Err(Error::DepthExceeded { path });
    }
    let content = loader(&path).map_err(|reason| Error::Load {
        path: path.clone(),
        reason,
    })?;
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    stack.push(path);

    // Pull include lines out first, leaving sentinels (never valid directive
    // text), so the includer's own slot pass cannot touch spliced content.
    let mut text = String::with_capacity(content.len());
    let mut expansions: Vec<(String, String)> = Vec::new();
    for (index, line) in content.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        let is_include_line = trimmed
            .strip_prefix("<!--")
            .is_some_and(|interior| directive_word(interior) == "include");
        if !is_include_line {
            text.push_str(line);
            continue;
        }
        let number = index + 1;
        let malformed = |reason: String| Error::MalformedInclude {
            line: number,
            reason,
        };
        let interior = trimmed
            .strip_prefix("<!--")
            .and_then(|rest| rest.strip_suffix("-->"))
            .ok_or_else(|| malformed("unterminated include comment".to_owned()))?;
        if interior.contains("-->") {
            return Err(malformed(
                "include line carries content after the directive".to_owned(),
            ));
        }
        let tail = interior.trim_start().trim_start_matches("include");
        let mut pairs = parse_named_args(tail).map_err(&malformed)?;
        let target_position = pairs
            .iter()
            .position(|(key, _)| key == "path")
            .ok_or_else(|| malformed("include requires path=\"...\"".to_owned()))?;
        let (_, target) = pairs.remove(target_position);
        let child_args: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let expanded = expand_at(&base.join(target), &child_args, loader, stack)?;
        let sentinel = format!("\u{0}INC{}\u{0}", expansions.len());
        text.push_str(&sentinel);
        if line.ends_with('\n') {
            text.push('\n');
        }
        expansions.push((sentinel, expanded));
    }

    let mut rendered = render(&text, args)?;
    for (sentinel, expanded) in expansions {
        rendered = rendered.replace(&sentinel, &expanded);
    }
    stack.pop();
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::{Error, expand, normalize, render};

    // ---- render: slot substitution ----

    #[test]
    fn substitutes_slots_including_repeats() {
        let out = render(
            "run <!-- slot name=\"cmd\" --> then <!-- slot name=\"cmd\" --> as <!-- slot name=\"who\" -->",
            &[("cmd", "ls"), ("who", "me")],
        )
        .expect("render");
        assert_eq!(out, "run ls then ls as me");
    }

    #[test]
    fn substitutes_inside_code_fences_and_prose() {
        let template = "```bash\n<!-- slot name=\"launch\" --> --state-file X &\n```\n\nPrint the output <!-- slot name=\"noun\" -->.\n";
        let out = render(
            template,
            &[("launch", "agent-gossip create"), ("noun", "block")],
        )
        .expect("render");
        assert_eq!(
            out,
            "```bash\nagent-gossip create --state-file X &\n```\n\nPrint the output block.\n"
        );
    }

    #[test]
    fn no_args_no_slots_is_identity() {
        let text = "plain text, no directives";
        assert_eq!(render(text, &[]).expect("render"), text);
    }

    // ---- render: strictness ----

    #[test]
    fn unknown_slot_errors() {
        assert_eq!(
            render("hello <!-- slot name=\"name\" -->", &[]),
            Err(Error::UnknownSlot {
                name: "name".to_owned()
            })
        );
    }

    #[test]
    fn unused_arg_errors() {
        assert_eq!(
            render("hello", &[("name", "world")]),
            Err(Error::UnusedArg {
                name: "name".to_owned()
            })
        );
    }

    #[test]
    fn malformed_slots_error_instead_of_passing_through() {
        for template in [
            "<!-- slot -->",                        // no args
            "<!-- slot noun -->",                   // bare token
            "<!-- slot label=\"x\" -->",            // wrong key
            "<!-- slot name=\"a\" name=\"b\" -->",  // duplicate key
            "<!-- slot name=\"a\" extra=\"b\" -->", // extra key
            "<!-- slot name=unquoted -->",          // unquoted value
            "<!-- slot name=\"bad\\n\" -->",        // invalid escape
            "<!-- slot name=\"unterminated",        // never closes
        ] {
            assert!(
                matches!(
                    render(template, &[("a", "x")]),
                    Err(Error::MalformedSlot { .. })
                ),
                "{template} must be a MalformedSlot error"
            );
        }
    }

    #[test]
    fn include_outside_its_own_line_errors_in_render() {
        assert!(matches!(
            render("text <!-- include path=\"x.md\" --> tail", &[]),
            Err(Error::MalformedInclude { .. })
        ));
    }

    // ---- render: passthrough ----

    #[test]
    fn ordinary_comments_and_literal_text_pass_through_byte_exact() {
        let corpus = "<!-- just a note --> single {brace}, {{name}}, JSON {\"peers\":{\"a\":1}}, shell ${VAR} and $(id -u)\n<!-- TODO: keep -->";
        assert_eq!(render(corpus, &[]).expect("render"), corpus);
    }

    #[test]
    fn unterminated_ordinary_comment_is_literal() {
        let corpus = "prose <!-- dangling note";
        assert_eq!(render(corpus, &[]).expect("render"), corpus);
    }

    #[test]
    fn deterministic() {
        let template = "a <!-- slot name=\"x\" --> b";
        let args = [("x", "1")];
        assert_eq!(render(template, &args), render(template, &args));
    }

    // ---- expand: include splicing ----

    fn map_loader(
        files: &[(&str, &str)],
    ) -> (
        HashMap<PathBuf, String>,
        impl FnMut(&Path) -> Result<String, String> + use<>,
    ) {
        let map: HashMap<PathBuf, String> = files
            .iter()
            .map(|(path, body)| (PathBuf::from(path), (*body).to_owned()))
            .collect();
        let cloned = map.clone();
        (map, move |path: &Path| {
            cloned
                .get(path)
                .cloned()
                .ok_or_else(|| "not found".to_owned())
        })
    }

    fn expand_files(files: &[(&str, &str)], root: &str) -> Result<String, Error> {
        let (_, mut loader) = map_loader(files);
        expand(Path::new(root), &[], &mut loader)
    }

    /// The directive line's own trailing newline survives after the spliced
    /// content, so a template's blank-line separation carries into the output.
    #[test]
    fn include_resolves_relative_to_the_importing_file() {
        let out = expand_files(
            &[
                ("skills/gossip-join/SKILL.md", "# join\n\n<!-- include path=\"workflow.md\" -->\n<!-- include path=\"../shared/loop.md\" -->\n"),
                ("skills/gossip-join/workflow.md", "## Workflow\n"),
                ("skills/shared/loop.md", "## Loop\n"),
            ],
            "skills/gossip-join/SKILL.md",
        )
        .expect("expand");
        assert_eq!(out, "# join\n\n## Workflow\n\n## Loop\n\n");
    }

    #[test]
    fn include_args_fill_the_included_files_slots() {
        let out = expand_files(
            &[
                (
                    "a/SKILL.md",
                    "<!-- include path=\"../shared/session.md\" launch=\"agent-gossip join \\\"{TARGET}\\\"\" noun=\"line\" -->",
                ),
                (
                    "shared/session.md",
                    "run: <!-- slot name=\"launch\" --> &\nprint the <!-- slot name=\"noun\" -->\n",
                ),
            ],
            "a/SKILL.md",
        )
        .expect("expand");
        assert_eq!(
            out,
            "run: agent-gossip join \"{TARGET}\" &\nprint the line\n"
        );
    }

    #[test]
    fn escaped_backslash_in_arg_value() {
        let out = expand_files(
            &[
                ("a.md", "<!-- include path=\"b.md\" v=\"back\\\\slash\" -->"),
                ("b.md", "<!-- slot name=\"v\" -->"),
            ],
            "a.md",
        )
        .expect("expand");
        assert_eq!(out, "back\\slash");
    }

    #[test]
    fn nested_includes_resolve_from_each_file() {
        let out = expand_files(
            &[
                ("top/a.md", "<!-- include path=\"../mid/b.md\" -->\n"),
                ("mid/b.md", "b(<!-- include path=\"../deep/c.md\" -->)\n"),
                ("deep/c.md", "c"),
            ],
            "top/a.md",
        );
        // `b.md`'s include is not alone on its line — that is an error, not a splice.
        assert!(matches!(out, Err(Error::MalformedInclude { .. })));

        let nested = expand_files(
            &[
                ("top/a.md", "<!-- include path=\"../mid/b.md\" -->"),
                (
                    "mid/b.md",
                    "before\n<!-- include path=\"../deep/c.md\" -->\nafter\n",
                ),
                ("deep/c.md", "c"),
            ],
            "top/a.md",
        )
        .expect("expand");
        assert_eq!(nested, "before\nc\nafter\n");
    }

    #[test]
    fn args_are_not_inherited_by_nested_includes() {
        let out = expand_files(
            &[
                ("a.md", "<!-- include path=\"b.md\" v=\"x\" -->\n"),
                (
                    "b.md",
                    "<!-- slot name=\"v\" -->\n<!-- include path=\"c.md\" -->\n",
                ),
                ("c.md", "<!-- slot name=\"v\" -->\n"),
            ],
            "a.md",
        );
        assert_eq!(
            out,
            Err(Error::UnknownSlot {
                name: "v".to_owned()
            })
        );
    }

    #[test]
    fn include_args_are_strict_per_call_site() {
        // Arg without a slot in the included file.
        let out = expand_files(
            &[
                ("a.md", "<!-- include path=\"b.md\" ghost=\"x\" -->\n"),
                ("b.md", "plain\n"),
            ],
            "a.md",
        );
        assert_eq!(
            out,
            Err(Error::UnusedArg {
                name: "ghost".to_owned()
            })
        );
    }

    #[test]
    fn malformed_includes_error() {
        for line in [
            "<!-- include -->",                             // no path
            "<!-- include workflow.md -->",                 // bare token
            "<!-- include path=workflow.md -->",            // unquoted
            "<!-- include path=\"a.md\" path=\"b.md\" -->", // duplicate key
            "<!-- include path=\"a.md\" v=\"bad\\x\" -->",  // invalid escape
            "<!-- include path=\"a.md\" --> trailing",      // content after
            "<!-- include path=\"a.md\"",                   // unterminated
        ] {
            let files = [("t.md", line), ("a.md", "ok")];
            assert!(
                matches!(
                    expand_files(&files, "t.md"),
                    Err(Error::MalformedInclude { .. })
                ),
                "{line} must be a MalformedInclude error"
            );
        }
    }

    #[test]
    fn include_cycles_are_detected() {
        let out = expand_files(
            &[
                ("a.md", "<!-- include path=\"b.md\" -->\n"),
                ("b.md", "<!-- include path=\"sub/../a.md\" -->\n"),
            ],
            "a.md",
        );
        assert_eq!(
            out,
            Err(Error::IncludeCycle {
                path: PathBuf::from("a.md")
            })
        );
    }

    #[test]
    fn include_depth_is_capped() {
        let chain: Vec<(String, String)> = (0..10)
            .map(|level| {
                (
                    format!("f{level}.md"),
                    format!("<!-- include path=\"f{}.md\" -->\n", level + 1),
                )
            })
            .collect();
        let files: Vec<(&str, &str)> = chain
            .iter()
            .map(|(path, body)| (path.as_str(), body.as_str()))
            .collect();
        assert!(matches!(
            expand_files(&files, "f0.md"),
            Err(Error::DepthExceeded { .. })
        ));
    }

    #[test]
    fn load_failure_carries_the_normalized_path() {
        let out = expand_files(
            &[("a.md", "<!-- include path=\"sub/../missing.md\" -->\n")],
            "a.md",
        );
        assert_eq!(
            out,
            Err(Error::Load {
                path: PathBuf::from("missing.md"),
                reason: "not found".to_owned()
            })
        );
    }

    /// A leading `..` has nothing to cancel, so it must survive — including the
    /// second one. Popping it would collapse `../../x` to `x`, silently reading a
    /// different file.
    #[test]
    fn leading_parent_dirs_accumulate() {
        assert_eq!(
            normalize(Path::new("../../x/y.md")),
            PathBuf::from("../../x/y.md")
        );
        assert_eq!(normalize(Path::new("../a/../b")), PathBuf::from("../b"));
        assert_eq!(normalize(Path::new("a/../../b")), PathBuf::from("../b"));
        // Root has no parent to climb to.
        assert_eq!(normalize(Path::new("/../a")), PathBuf::from("/a"));
    }

    #[test]
    fn indented_include_lines_work_and_comments_survive() {
        let out = expand_files(
            &[
                ("a.md", "<!-- keep me -->\n  <!-- include path=\"b.md\" -->"),
                ("b.md", "spliced\n"),
            ],
            "a.md",
        )
        .expect("expand");
        assert_eq!(out, "<!-- keep me -->\nspliced\n");
    }
}
