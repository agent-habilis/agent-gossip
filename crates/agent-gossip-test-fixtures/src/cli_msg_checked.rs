use crate::cli_msg_raw;

/// [`cli_msg_raw`] + assert success + trim stdout.
pub fn cli_msg_checked(mesh: &str, nickname: &str, to: &str, body: &str) -> String {
    let out = cli_msg_raw(mesh, nickname, to, body);
    assert!(
        out.status.success(),
        "a2a msg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
