use anyhow::{Context, Result, bail};

use crate::protocol::crypto::Password;

/// Resolve a clap optional-value `--password` flag (absent / bare / valued)
/// into a [`Password`]. Bare ⇒ a hidden prompt on the controlling TTY —
/// `/dev/tty`, never stdin/stdout, so it composes with `pipe listen < file`
/// and `pipe connect > file`. `confirm` re-prompts and compares (creation
/// paths: a typo would mint an unjoinable swarm or an unredeemable ticket).
///
/// `no_prompt` (non-interactive / `--output json` frontends) makes a bare
/// flag a hard error instead of a hang waiting on a TTY an agent doesn't have.
#[expect(
    clippy::option_option,
    reason = "clap optional-value flag: absent/bare/valued are three distinct password states"
)]
pub(crate) fn resolve_password(
    flag: Option<Option<String>>,
    confirm: bool,
    no_prompt: bool,
) -> Result<Option<Password>> {
    match flag {
        None => Ok(None),
        Some(Some(value)) => validated(value).map(Some),
        Some(None) => {
            if no_prompt {
                bail!("--password needs an inline value here: --password=<pw> (no TTY to prompt)");
            }
            let first = prompt("password: ")?;
            if confirm {
                let second = prompt("confirm password: ")?;
                if first != second {
                    bail!("passwords do not match");
                }
            }
            validated(first).map(Some)
        }
    }
}

/// The target (a `🐝…` id or ticket) is password-protected but the flag was
/// absent: prompt on a TTY, or bail with a crisp instruction. `what` names
/// the artifact for the error ("swarm", "ticket"). A failed prompt (no
/// controlling TTY — e.g. a scripted run) still names the requirement.
pub(crate) fn require_password(no_prompt: bool, what: &str) -> Result<Password> {
    if no_prompt {
        bail!("this {what} is password-protected — pass --password");
    }
    let value = prompt(&format!("{what} password: "))
        .with_context(|| format!("this {what} is password-protected — pass --password=<pw>"))?;
    validated(value)
}

fn prompt(text: &str) -> Result<String> {
    // rpassword reads AND writes /dev/tty with ECHO off + ICANON on — the
    // exact termios state Ghostty/WezTerm/iTerm2 detect to show their
    // password-input indicator (and enable macOS secure input).
    rpassword::prompt_password(text)
        .context("cannot prompt for a password (no TTY) — pass --password=<pw>")
}

fn validated(value: String) -> Result<Password> {
    if value.trim().is_empty() {
        bail!("password must not be empty");
    }
    Ok(Password::new(value))
}

#[cfg(test)]
mod tests {
    use super::resolve_password;

    #[test]
    fn absent_flag_is_no_password() {
        assert!(resolve_password(None, false, true).unwrap().is_none());
    }

    #[test]
    fn inline_value_needs_no_tty() {
        let password = resolve_password(Some(Some("hunter2".to_owned())), true, true)
            .unwrap()
            .unwrap();
        assert_eq!(password.as_str(), "hunter2");
    }

    #[test]
    fn empty_and_whitespace_values_are_rejected() {
        assert!(resolve_password(Some(Some(String::new())), false, true).is_err());
        assert!(resolve_password(Some(Some("   ".to_owned())), false, true).is_err());
    }

    #[test]
    fn bare_flag_without_a_tty_context_is_a_hard_error() {
        let error = resolve_password(Some(None), false, true).unwrap_err();
        assert!(
            error.to_string().contains("--password=<pw>"),
            "got: {error}"
        );
    }

    #[test]
    fn require_password_without_a_tty_context_names_the_artifact() {
        let error = super::require_password(true, "ticket").unwrap_err();
        assert!(
            error.to_string().contains("ticket is password-protected"),
            "got: {error}"
        );
    }
}
