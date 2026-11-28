//! Wire-contract test for `agent-gossip man`: the binary must print its embedded
//! manual to stdout, exit 0, and render the canonical man-page sections.

use agent_gossip_test_fixtures as common;

#[test]
fn man_prints_manual_to_stdout() {
    let output = common::test_cmd()
        .arg("man")
        .output()
        .expect("failed to run `agent-gossip man`");

    assert!(
        output.status.success(),
        "`agent-gossip man` should exit 0, got {:?}",
        output.status
    );

    let stdout =
        String::from_utf8(output.stdout).expect("the manual is embedded text from a source file");

    // Renders the man page: the canonical sections + key agent material.
    for marker in [
        "NAME",
        "SYNOPSIS",
        "DESCRIPTION",
        "COMMANDS",
        "JSON EVENTS",
        "JOIN HORIZON",
        "EXAMPLES",
        "EXIT STATUS",
        "agent-gossip man",
        "--nickname",
        "ping_report",
        "create",
        "join",
        "poll",
        "discover",
        "task",
        "peers",
        "leave",
        "session",
    ] {
        assert!(
            stdout.contains(marker),
            "manual missing expected marker {marker:?}"
        );
    }
}

/// The `agent-gossip man` text, asserted to be non-empty so a failed run cannot pass a scan.
fn manual_text() -> String {
    let output = common::test_cmd()
        .arg("man")
        .output()
        .expect("failed to run `agent-gossip man`");
    assert!(output.status.success(), "`agent-gossip man` should exit 0");
    let text =
        String::from_utf8(output.stdout).expect("the manual is embedded text from a source file");
    assert!(
        !text.trim().is_empty(),
        "`agent-gossip man` produced no text"
    );
    text
}

/// The block of the manual that starts at the line `start` and runs to the first line that is
/// empty after a line with "}" — the JSON example under a heading.
fn example_after<'text>(text: &'text str, start: &str) -> &'text str {
    let from = text
        .find(start)
        .unwrap_or_else(|| panic!("the manual has no example that starts with {start}"));
    let rest = &text[from..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    &rest[..end]
}

/// A directed line names its addressee by key as well as by name: the nickname is a label that two
/// members can share. The manual shows the field in the examples that have an addressee, and
/// describes it in the field list.
#[test]
fn the_manual_documents_to_pubkey_where_a_line_has_an_addressee() {
    let manual = manual_text();
    let examples = [
        r#"{"event":"message","id":"uuid","type":"msg""#,
        r#"{"event":"task","id":"<uuid>""#,
        r#"{"event":"task_progress","id":"<uuid>""#,
    ];
    for start in examples {
        let example = example_after(&manual, start);
        assert!(
            example.contains(r#""to_pubkey""#),
            "the example {start} shows no to_pubkey:\n{example}"
        );
    }
    let field_list = manual
        .lines()
        .any(|line| line.trim_start().starts_with("to_pubkey"));
    assert!(field_list, "the field list does not describe to_pubkey");
}

/// The gossip wire version in the manual is the version of the engine that the app pins.
#[test]
fn the_manual_names_wire_version_18() {
    // Whitespace is normalized, so a line wrap in the manual does not break the phrase.
    let text = manual_text();
    let words: Vec<&str> = text.split_whitespace().collect();
    let manual = words.join(" ");
    assert!(
        manual.contains("wire version 18.0)"),
        "the iroh-gossip paragraph does not name wire version 18.0"
    );
}
