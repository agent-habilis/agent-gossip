use anyhow::Result;
use habilis_network::protocol::Nickname;

use super::msg_response::MsgResponse;
use crate::a2a::ipc_command::IpcCommand;

fn reply_is_ok(response: &str) -> Result<bool> {
    let parsed: MsgResponse = serde_json::from_str(response)?;
    Ok(parsed.ok)
}

/// The reply of a command that the daemon accepted. A refusal becomes an error with the text
/// the daemon gave.
pub(super) fn accepted(response: &str) -> Result<MsgResponse> {
    let parsed: MsgResponse = serde_json::from_str(response)?;
    if !parsed.ok {
        anyhow::bail!(
            "{}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    Ok(parsed)
}

/// Send `command` and print the raw reply.
///
/// A refused reply must not exit 0: a shell-driven agent reads the exit code to
/// tell an applied change from a rejected one, and an `{"ok":false}` that exits
/// 0 reads as success → silent desync. The raw JSON is printed first for
/// `--output json` consumers; the exit code is the scriptable signal.
pub(super) async fn send_printed(command: &IpcCommand, nickname: &Nickname) -> Result<()> {
    let response = super::send::send(command, nickname).await?;
    println!("{response}");
    if !reply_is_ok(&response)? {
        std::process::exit(1);
    }
    Ok(())
}

/// Send `command` and return the reply the daemon accepted. A refused reply is
/// printed and ends the process with exit code 1.
pub(super) async fn send_accepted(command: &IpcCommand, nickname: &Nickname) -> Result<String> {
    let response = super::send::send(command, nickname).await?;
    if !reply_is_ok(&response)? {
        println!("{response}");
        std::process::exit(1);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    /// `topology --draw` exits 1 on a reply that is not ok, for the topology reply and for the
    /// peers reply alike. The exit call itself needs a daemon that answers one query and not the
    /// other, so the test pins the decision.
    #[test]
    fn a_reply_that_says_not_ok_fails_the_command() {
        let failed = r#"{"ok":false,"error":"no such gossip"}"#;
        let fine = r#"{"ok":true,"peers":[]}"#;

        assert!(
            !super::reply_is_ok(failed).expect("a reply is JSON"),
            "the daemon refused the query, so the command exits 1"
        );
        assert!(
            super::reply_is_ok(fine).expect("a reply is JSON"),
            "the daemon answered, so the command goes on to the drawing"
        );
    }
}
