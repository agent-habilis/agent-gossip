use crate::cli_message_raw;

/// [`cli_message_raw`] + assert success + trim stdout.
///
/// # Panics
///
/// Panics if the CLI process fails to spawn or exits with an error.
#[must_use]
pub fn cli_message_checked(mesh: &str, nickname: &str, body: &str) -> String {
    let out = cli_message_raw(mesh, nickname, body);
    assert!(
        out.status.success(),
        "a2a broadcast failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
