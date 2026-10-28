use crate::cli_message_raw;

/// [`cli_message_raw`] + trim stdout. No success assertion.
pub fn cli_message(mesh: &str, nickname: &str, body: &str) -> String {
    let out = cli_message_raw(mesh, nickname, body);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
