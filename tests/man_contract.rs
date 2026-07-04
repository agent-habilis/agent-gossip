//! Wire-contract test for `agent-gossip man`: the binary must print its embedded
//! manual to stdout, exit 0, and render the canonical man-page sections.

mod common;

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

    let stdout = String::from_utf8(output.stdout).expect("manual is UTF-8");

    // Renders the man page: the canonical sections + key agent material.
    for marker in [
        "NAME",
        "SYNOPSIS",
        "DESCRIPTION",
        "COMMANDS",
        "INTERACTIVE MODE",
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
