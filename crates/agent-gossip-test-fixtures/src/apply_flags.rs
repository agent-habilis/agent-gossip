use std::process::Command;

/// Apply `(flag, value)` tuning pairs to a spawn `Command`. `RUST_LOG` (a
/// kept standard convention, not app config) is set as an **env var**; every
/// other pair becomes a CLI flag (empty value ⇒ bare flag, e.g.
/// `("--directory-private", "")`). Use for the `Node` spawns that may carry a
/// `RUST_LOG` debug filter.
pub(crate) fn apply_flags(cmd: &mut Command, pairs: &[(&str, &str)]) {
    for (flag, value) in pairs {
        if *flag == "RUST_LOG" {
            cmd.env("RUST_LOG", value);
        } else {
            cmd.arg(flag);
            if !value.is_empty() {
                cmd.arg(value);
            }
        }
    }
}
