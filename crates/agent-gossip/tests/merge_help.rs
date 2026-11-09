//! The field help of the `--merge` argument of `state merge` and `meta merge`.

/// `--help` prints the field help, so it must say what the action help says: the top-level merge is a
/// JSON object. "Any JSON value" tells a user that a scalar is allowed, and the write is refused.
#[test]
fn the_merge_field_help_says_the_top_level_is_an_object() {
    let command = agent_gossip::cli_command();
    for (group, subcommand) in [("state", "merge"), ("meta", "merge")] {
        let merge = command
            .find_subcommand(group)
            .and_then(|group| group.find_subcommand(subcommand))
            .unwrap_or_else(|| panic!("the CLI has `{group} {subcommand}`"));
        let field = merge
            .get_arguments()
            .find(|arg| arg.get_id() == "merge")
            .unwrap_or_else(|| panic!("`{group} {subcommand}` takes --merge"));
        let help = field
            .get_help()
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(
            help.contains("JSON object"),
            "the --merge help of `{group} {subcommand}` says the top level is an object: {help}"
        );
        assert!(
            !help.contains("any JSON value"),
            "the --merge help of `{group} {subcommand}` no longer allows any value: {help}"
        );
    }
}
