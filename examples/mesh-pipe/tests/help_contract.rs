//! `--help` must describe ids the way the binary prints them: bare Base58.
//!
//! mesh-pipe spells the id format literally in its clap `value_name`s and doc
//! comments — `--help` text cannot interpolate a const — so a format change has
//! to be swept by hand here. It was not, once: this crate advertised a stale
//! mesh sigil in `--help` while the binary printed a different one, and nothing
//! failed, because the sigil was display text no code path parses. Ids carry no
//! sigil at all now, so the guard is inverted: help must name no glyph and no
//! `://`.

use std::process::Command;

fn help_for(subcommand: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_mesh-pipe"))
        .args([subcommand, "--help"])
        .output()
        .expect("run mesh-pipe --help");
    assert!(
        output.status.success(),
        "`mesh-pipe {subcommand} --help` should exit 0, got {:?}",
        output.status
    );
    String::from_utf8(output.stdout).expect("help is UTF-8")
}

#[test]
fn help_advertises_bare_ids() {
    for subcommand in ["listen", "connect"] {
        let help = help_for(subcommand);
        assert!(
            !help.contains("://"),
            "`mesh-pipe {subcommand} --help` still shows a `://` id form:\n{help}"
        );
        for glyph in ['💬', '🎟', '🤖'] {
            assert!(
                !help.contains(glyph),
                "`mesh-pipe {subcommand} --help` still names the {glyph:?} sigil — \
                 ids are bare Base58 and this crate's help text did not follow:\n{help}"
            );
        }
    }
}
