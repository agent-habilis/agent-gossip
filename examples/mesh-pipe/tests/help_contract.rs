//! `--help` must name the *current* mesh sigil.
//!
//! mesh-pipe spells the glyph literally in its clap `value_name`s and doc
//! comments — `--help` text cannot interpolate a const — so a re-glyph has to be
//! swept by hand here. It was not, once: this crate advertised the previous
//! sigil in `--help` while the binary printed the new one, and nothing failed,
//! because the glyph is display text no code path parses. This test is the
//! failure that was missing.

use std::process::Command;

use agent_habilis_mesh::util::consts::MESH_GLYPH;

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
fn help_names_the_current_mesh_glyph() {
    for subcommand in ["listen", "connect"] {
        let help = help_for(subcommand);
        assert!(
            help.contains(MESH_GLYPH),
            "`mesh-pipe {subcommand} --help` never names the mesh sigil \
             {MESH_GLYPH:?} — the glyph moved and this crate's help text did \
             not follow:\n{help}"
        );
    }
}
