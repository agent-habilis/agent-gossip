use anyhow::{Context, Result, bail};

use crate::protocol::crypto::Password;

/// The `default_missing_value` a bare `--password` (no `=value`) resolves to.
/// A lone NUL can't be typed on a command line — the shell terminates the
/// argument at it — so it can never collide with a real inline password, and
/// it stays distinct from an explicit empty `--password=` (which remains an
/// "empty password" error).
pub(crate) const BARE: &str = "\0";

/// A `--password` optional-value flag resolved by clap: bare `--password`
/// (prompt on a TTY) vs `--password=<pw>` (inline). The *absent* flag is
/// `Option::None` at the field, so this type models only the two present
/// forms — replacing the former `Option<Option<String>>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PasswordFlag {
    /// Bare `--password`: prompt on the controlling TTY.
    Prompt,
    /// `--password=<pw>`: the inline value (possibly empty, which
    /// [`resolve_password`] rejects).
    Inline(String),
}

impl std::str::FromStr for PasswordFlag {
    type Err = std::convert::Infallible;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(if text == BARE {
            PasswordFlag::Prompt
        } else {
            PasswordFlag::Inline(text.to_owned())
        })
    }
}

/// Resolve a clap optional-value `--password` flag (absent / bare / valued)
/// into a [`Password`]. Bare ⇒ a hidden prompt on the controlling TTY —
/// `/dev/tty`, never stdin/stdout, so it never collides with a command's
/// stdout/stdin data stream. `confirm` re-prompts and compares (creation
/// paths: a typo would mint an unjoinable swarm or an unredeemable ticket).
///
/// `no_prompt` (non-interactive / `--output json` frontends) makes a bare
/// flag a hard error instead of a hang waiting on a TTY an agent doesn't have.
pub(crate) fn resolve_password(
    flag: Option<PasswordFlag>,
    confirm: bool,
    no_prompt: bool,
) -> Result<Option<Password>> {
    match flag {
        None => Ok(None),
        Some(PasswordFlag::Inline(value)) => validated(value).map(Some),
        Some(PasswordFlag::Prompt) => {
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

/// The target (a `💬…` id or ticket) is password-protected but the flag was
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
    use super::{PasswordFlag, resolve_password};

    #[test]
    fn absent_flag_is_no_password() {
        assert!(resolve_password(None, false, true).unwrap().is_none());
    }

    #[test]
    fn inline_value_needs_no_tty() {
        let password =
            resolve_password(Some(PasswordFlag::Inline("hunter2".to_owned())), true, true)
                .unwrap()
                .unwrap();
        assert_eq!(password.as_str(), "hunter2");
    }

    #[test]
    fn empty_and_whitespace_values_are_rejected() {
        assert!(resolve_password(Some(PasswordFlag::Inline(String::new())), false, true).is_err());
        assert!(
            resolve_password(Some(PasswordFlag::Inline("   ".to_owned())), false, true).is_err()
        );
    }

    #[test]
    fn bare_flag_without_a_tty_context_is_a_hard_error() {
        let error = resolve_password(Some(PasswordFlag::Prompt), false, true).unwrap_err();
        assert!(
            error.to_string().contains("--password=<pw>"),
            "got: {error}"
        );
    }

    #[test]
    fn bare_sentinel_parses_to_prompt() {
        assert!(matches!(
            super::BARE.parse::<PasswordFlag>(),
            Ok(PasswordFlag::Prompt)
        ));
        assert!(matches!(
            "hunter2".parse::<PasswordFlag>(),
            Ok(PasswordFlag::Inline(value)) if value == "hunter2"
        ));
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
