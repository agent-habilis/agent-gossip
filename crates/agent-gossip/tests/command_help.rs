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
