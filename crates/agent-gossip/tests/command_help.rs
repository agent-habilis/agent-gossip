//! The help of the `peers` and `leave` commands and of `--state-file`.

fn long_help(command: &clap::Command) -> String {
    let about = command.get_long_about().or(command.get_about());
    about.map(ToString::to_string).unwrap_or_default()
}

/// `peer_count` is the live gossip size, so a reader that takes it for the number of other peers is
/// one too high. The help of the command that prints it says what it counts.
#[test]
fn peers_help_says_that_peer_count_is_the_active_peers_plus_self() {
    let command = agent_gossip::cli_command();
    let peers = command
        .find_subcommand("peers")
        .expect("the CLI has the `peers` command");
    let help = long_help(peers);
    assert!(
        help.contains("active peers plus self"),
        "`peers --help` names peer_count and not what it counts: {help}"
    );
}

/// `leave` finds a daemon through its state file, and through its socket when the state file is
/// outside the runtime base, so the help names both.
#[test]
fn leave_help_names_the_sockets_beside_the_state_files() {
    let command = agent_gossip::cli_command();
    let leave = command
        .find_subcommand("leave")
        .expect("the CLI has the `leave` command");
    let help = long_help(leave);
    assert!(
        help.contains("sockets"),
        "`leave --help` says that discovery reads state files only: {help}"
    );
}

/// A state file outside the runtime base is found through its daemon's socket, so a daemon that
/// died without cleaning up leaves a file that nothing names. The help of the flag says so.
#[test]
fn the_state_file_help_names_the_limit_of_a_path_outside_the_runtime_base() {
    let command = agent_gossip::cli_command();
    let create = command
        .find_subcommand("create")
        .expect("the CLI has the `create` command");
    let flag = create
        .get_arguments()
        .find(|arg| arg.get_id() == "state_file")
        .expect("`create` takes --state-file");
    let help = flag.get_help().map(ToString::to_string).unwrap_or_default();
    assert!(
        help.contains("outside the runtime base"),
        "the --state-file help is silent about a path that no discovery can clean: {help}"
    );
}

const MANUAL: &str = include_str!("../../../docs/manual.txt");

/// The manual says what a stop leaves behind and how long it takes, so a reader does not take a
/// lock file for a stuck daemon or a 4 s stop for a hang.
#[test]
fn the_manual_says_what_a_stop_leaves_and_how_long_it_takes() {
    for (needle, why) in [
        (
            ".key.lock",
            "the lock files that stay beside the state file",
        ),
        ("flock", "why the lock files are not deleted"),
        (
            "up to 6 s",
            "the longest stop when a peer does not acknowledge the close",
        ),
        ("up to 8 s", "how long leave waits for a stop"),
        ("7 days", "when the tracing log of a dead member goes"),
        ("0 or 1", "that a --session-pid of init is refused"),
        (
            "more than one session",
            "that a real pid above several sessions still selects them all",
        ),
        (
            "not backfilled",
            "that a node which is away does not get the broadcasts it missed",
        ),
        (
            "is the state of the agent session",
            "the contract of the sessions folder",
        ),
        (
            "which is not an ancestor",
            "that a session file with no ancestor pid starts a new member",
        ),
        (
            "a new member identity was made",
            "the warning of a session file that starts a new member",
        ),
        (
            "pid>.stderr file",
            "where the warning lands in the launch of the skills",
        ),
        (
            "The member key is the stable identity of a node",
            "the stable identity beside the pubkey of one run",
        ),
        (
            "protected only while its holder is present",
            "the limit of the nickname protection",
        ),
        (
            "member proves which key a session holds",
            "that a member is a proof of a key and not of a node",
        ),
        (
            "A clean leave of a run with the",
            "that the default path with no owner loses the key",
        ),
        (
            "own_member, the member key of the",
            "the member key of the node itself in the peers reply",
        ),
        (
            "members maps each slot of document",
            "the member key of each slot in the meta get reply",
        ),
        (
            "Only the beacon publishes to the DHT",
            "who publishes to the DHT",
        ),
        (
            "no IP address",
            "that a record with a relay lookup holds no IP address",
        ),
        (
            "in the public DHT",
            "that a DHT-only gossip puts the IP addresses of the beacon in the public DHT",
        ),
    ] {
        assert!(
            MANUAL.contains(needle),
            "docs/manual.txt does not name {why}: it has no {needle:?}"
        );
    }
}

/// The DHT is public. A user who picks the lookups must read in the help of the flag what the beacon
/// puts there, so the choice between a relay lookup and a DHT-only gossip is an informed one.
#[test]
fn the_lookup_help_says_what_the_dht_publishes() {
    let command = agent_gossip::cli_command();
    let create = command
        .find_subcommand("create")
        .expect("the CLI has the `create` command");
    let flag = create
        .get_arguments()
        .find(|arg| arg.get_id() == "lookup")
        .expect("`create` takes --lookup");
    let help = flag.get_help().map(ToString::to_string).unwrap_or_default();
    for needle in [
        "Only the beacon publishes to the DHT",
        "no IP address",
        "in the public DHT",
    ] {
        assert!(
            help.contains(needle),
            "the --lookup help does not say what the DHT publishes: it has no {needle:?}"
        );
    }
}
