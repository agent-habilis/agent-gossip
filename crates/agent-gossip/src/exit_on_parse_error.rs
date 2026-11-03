/// Exit on a parse error, as `clap::Error::exit` does, but with 1 for
/// `bell-check`. On Windows the Stop hook runs `bell-check` in exec form, with
/// no `|| true`, and a Stop hook that exits 2 blocks the turn. A usage error,
/// such as an `auto` with no agent above it or a binary too old for `auto`,
/// must not hold the agent in its turn.
pub(crate) fn exit_on_parse_error(error: &clap::Error) -> ! {
    let bell_check = std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "bell-check");
    if bell_check && error.use_stderr() {
        let _ = error.print();
        std::process::exit(1);
    }
    error.exit()
}
