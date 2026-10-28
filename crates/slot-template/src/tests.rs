use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::expand::expand;
use crate::normalize::normalize;
use crate::render::render;

// ---- render: slot substitution ----

#[test]
fn substitutes_slots_including_repeats() {
    let out = render(
            "run <!-- slot name=\"cmd\" --> then <!-- slot name=\"cmd\" --> as <!-- slot name=\"who\" -->",
            &[("cmd", "ls"), ("who", "me")],
        )
        .expect("render");
    assert_eq!(
        out, "run ls then ls as me",
        "every slot, repeated or not, must be replaced by its value"
    );
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
        out, "```bash\nagent-gossip create --state-file X &\n```\n\nPrint the output block.\n",
        "slots inside a code fence and in prose must both be filled"
    );
}

#[test]
fn no_args_no_slots_is_identity() {
    let text = "plain text, no directives";
    assert_eq!(
        render(text, &[]).expect("render"),
        text,
        "text without directives must come back unchanged"
    );
}

// ---- render: strictness ----

#[test]
fn unknown_slot_errors() {
    assert_eq!(
        render("hello <!-- slot name=\"name\" -->", &[]),
        Err(Error::UnknownSlot {
            name: "name".to_owned()
        }),
        "a slot with no matching arg must fail with UnknownSlot"
    );
}

#[test]
fn unused_arg_errors() {
    assert_eq!(
        render("hello", &[("name", "world")]),
        Err(Error::UnusedArg {
            name: "name".to_owned()
        }),
        "an arg with no matching slot must fail with UnusedArg"
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
    assert!(
        matches!(
            render("text <!-- include path=\"x.md\" --> tail", &[]),
            Err(Error::MalformedInclude { .. })
        ),
        "an include that shares its line with text must fail in render"
    );
}

// ---- render: passthrough ----

#[test]
fn ordinary_comments_and_literal_text_pass_through_byte_exact() {
    let corpus = "<!-- just a note --> single {brace}, {{name}}, JSON {\"peers\":{\"a\":1}}, shell ${VAR} and $(id -u)\n<!-- TODO: keep -->";
    assert_eq!(
        render(corpus, &[]).expect("render"),
        corpus,
        "ordinary comments and literal braces must pass through byte for byte"
    );
}

#[test]
fn unterminated_ordinary_comment_is_literal() {
    let corpus = "prose <!-- dangling note";
    assert_eq!(
        render(corpus, &[]).expect("render"),
        corpus,
        "an unterminated comment must stay literal text"
    );
}

#[test]
fn deterministic() {
    let template = "a <!-- slot name=\"x\" --> b";
    let args = [("x", "1")];
    assert_eq!(
        render(template, &args),
        render(template, &args),
        "two renders of one template must give one result"
    );
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
    assert_eq!(
        out, "# join\n\n## Workflow\n\n## Loop\n\n",
        "each include must resolve from the folder of the file that holds it"
    );
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
        out, "run: agent-gossip join \"{TARGET}\" &\nprint the line\n",
        "include args must fill the slots of the included file, with escapes decoded"
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
    assert_eq!(
        out, "back\\slash",
        "an escaped backslash in an arg value must decode to one backslash"
    );
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
    assert!(
        matches!(out, Err(Error::MalformedInclude { .. })),
        "an include inside a line of text must fail, got {out:?}"
    );

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
    assert_eq!(
        nested, "before\nc\nafter\n",
        "a nested include must splice from the folder of its own file"
    );
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
        }),
        "a nested include must not see the args of its parent"
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
        }),
        "an include arg with no slot in the included file must fail"
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
        }),
        "a file that includes itself through a detour must be a cycle"
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
    assert!(
        matches!(
            expand_files(&files, "f0.md"),
            Err(Error::DepthExceeded { .. })
        ),
        "a chain of ten includes must exceed the depth cap"
    );
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
        }),
        "a missing file must be reported by its normalized path"
    );
}

/// A leading `..` has nothing to cancel, so it must survive — including the
/// second one. Popping it would collapse `../../x` to `x`, silently reading a
/// different file.
#[test]
fn leading_parent_dirs_accumulate() {
    assert_eq!(
        normalize(Path::new("../../x/y.md")),
        PathBuf::from("../../x/y.md"),
        "two leading parent dirs must both survive"
    );
    assert_eq!(
        normalize(Path::new("../a/../b")),
        PathBuf::from("../b"),
        "a parent dir after a name must cancel it"
    );
    assert_eq!(
        normalize(Path::new("a/../../b")),
        PathBuf::from("../b"),
        "a parent dir that has nothing left to cancel must stay"
    );
    // Root has no parent to climb to.
    assert_eq!(
        normalize(Path::new("/../a")),
        PathBuf::from("/a"),
        "the root has no parent, so a parent dir under it must vanish"
    );
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
    assert_eq!(
        out, "<!-- keep me -->\nspliced\n",
        "an ordinary comment must survive an include on an indented line"
    );
}
