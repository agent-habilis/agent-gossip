pub(super) fn announce(status: &str, command: &str) {
    tracing::info!("{status}");
    println!("{command}");
}
