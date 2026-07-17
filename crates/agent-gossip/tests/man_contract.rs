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

    let stdout = String::from_utf8(output.stdout).expect("manual is UTF-8");

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

/// The manual is the one place the old vocabulary can rot back in unnoticed:
/// it is 1600 lines of prose with no compiler behind it, and it was already
/// ~250 uses of "square" out of step with the code before the rename. The
/// session noun is **room** (see `docs/glossary.md`, `mesh hash`); the product
/// is **agent-gossip**. Neither spelling of the old name may reappear.
#[test]
fn manual_carries_no_pre_rename_vocabulary() {
    let output = common::test_cmd()
        .arg("man")
        .output()
        .expect("failed to run `agent-gossip man`");
    // Assert success first: a `man` that errored to empty stdout would make the
    // offender scan below pass vacuously, silently disarming this guard.
    assert!(
        output.status.success(),
        "`agent-gossip man` should exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("manual is UTF-8");
    assert!(
        !stdout.trim().is_empty(),
        "`agent-gossip man` produced no output"
    );

    let offenders: Vec<_> = stdout
        .lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains("square"))
        .map(|(number, line)| format!("  {}: {line}", number + 1))
        .collect();

    assert!(
        offenders.is_empty(),
        "manual reintroduced pre-rename vocabulary on {} line(s):\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
