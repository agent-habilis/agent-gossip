use anyhow::{Result, bail};
use clap::builder::{StringValueParser, TypedValueParser};
use habilis_network::protocol::Password;

/// The `default_missing_value` a bare `--password` (no `=value`) resolves to.
/// A lone NUL can't be typed on a command line — the shell terminates the
/// argument at it — so it can never collide with a real inline password, and
/// it stays distinct from an explicit empty `--password=` (which remains an
/// "empty password" error).
pub(crate) const BARE: &str = "\0";

/// A `--password` optional-value flag resolved by clap: bare `--password` vs
/// `--password=<pw>` (inline). The *absent* flag is `Option::None` at the
/// field, so this type models only the two present forms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PasswordFlag {
    /// Bare `--password` with no value. The CLI is agent-driven with no TTY to
    /// prompt, so a bare flag is a hard error — [`resolve_password`] rejects it.
    Bare,
    /// `--password=<pw>`: the inline value (possibly empty, which
    /// [`resolve_password`] rejects).
    Inline(String),
}

fn parse_password_flag(text: String) -> PasswordFlag {
    if text == BARE {
        PasswordFlag::Bare
    } else {
        PasswordFlag::Inline(text)
    }
}

pub(crate) fn password_flag_parser() -> impl TypedValueParser<Value = PasswordFlag> {
    StringValueParser::new().map(parse_password_flag)
}

/// Resolve a clap optional-value `--password` flag (absent / bare / valued)
/// into a [`Password`]. A bare `--password` is a hard error — the CLI is
/// agent-driven with no TTY to prompt, so the value must be passed inline.
pub(crate) fn resolve_password(flag: Option<PasswordFlag>) -> Result<Option<Password>> {
    match flag {
        None => Ok(None),
        Some(PasswordFlag::Inline(value)) => validated(value).map(Some),
        Some(PasswordFlag::Bare) => bail!("--password needs an inline value: --password=<pw>"),
    }
}

/// The target (a mesh id or ticket) is password-protected but the flag
/// was absent. `what` names the artifact for the error ("gossip", "ticket").
///
/// # Errors
/// Always errors: a password-protected target needs an explicit
/// `--password=<pw>` (there is no interactive prompt).
pub(crate) fn require_password(what: &str) -> Result<Password> {
    bail!("this {what} is password-protected — pass --password=<pw>")
}

fn validated(value: String) -> Result<Password> {
    if value.trim().is_empty() {
        bail!("password must not be empty");
    }
    Ok(Password::new(value))
}

#[cfg(test)]
mod tests {
    use super::{PasswordFlag, parse_password_flag, resolve_password};

    #[test]
    fn absent_flag_is_no_password() {
        assert!(
            resolve_password(None)
                .expect("no password source is set in this test")
                .is_none(),
            "no password flag resolves to no password"
        );
    }

    #[test]
    fn inline_value_resolves() {
        let password = resolve_password(Some(PasswordFlag::Inline("hunter2".to_owned())))
            .expect("no password source is set in this test")
            .expect("no password source is set in this test");
        assert_eq!(
            password.as_str(),
            "hunter2",
            "the inline password is kept as given"
        );
    }

    #[test]
    fn empty_and_whitespace_values_are_rejected() {
        assert!(
            resolve_password(Some(PasswordFlag::Inline(String::new()))).is_err(),
            "an empty inline password is rejected"
        );
        assert!(
            resolve_password(Some(PasswordFlag::Inline("   ".to_owned()))).is_err(),
            "a whitespace-only inline password is rejected"
        );
    }

    #[test]
    fn bare_flag_is_a_hard_error() {
        let error = resolve_password(Some(PasswordFlag::Bare))
            .expect_err("a bare flag has no value to read");
        assert!(
            error.to_string().contains("--password=<pw>"),
            "got: {error}"
        );
    }

    #[test]
    fn bare_sentinel_parses_to_bare() {
        assert!(
            parse_password_flag(super::BARE.to_owned()) == PasswordFlag::Bare,
            "the bare sentinel parses to the bare flag"
        );
        assert!(
            parse_password_flag("hunter2".to_owned()) == PasswordFlag::Inline("hunter2".to_owned()),
            "a plain value parses to an inline password"
        );
    }

    #[test]
    fn require_password_names_the_artifact() {
        let error =
            super::require_password("ticket").expect_err("no password source is set in this test");
        assert!(
            error.to_string().contains("ticket is password-protected"),
            "got: {error}"
        );
    }
}
