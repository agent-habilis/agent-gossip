//! Every `tracing` target this crate emits under must be pinned in
//! [`agent_gossip::APP_LOG_PINS`].
//!
//! A release build's base level is `error`, so an unpinned target compiles,
//! passes review, works in a debug build, and is silently dropped from every
//! optimized one. That is not hypothetical: engine-side, losing the pins once
//! made a reliability test fail 100% under `--profile ci` while passing locally,
//! and shipped release binaries with no beacon diagnostics at all. This test is
//! the app-side guard for the same footgun.

use std::path::{Path, PathBuf};

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_emitted_app_target_is_pinned() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "found no sources under {}",
        src.display()
    );

    let pins = agent_gossip::APP_LOG_PINS;
    let mut unpinned = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source");
        for (index, line) in text.lines().enumerate() {
            // `target: "agent_gossip::<subsystem>"` — the app's own targets.
            let Some(rest) = line.split_once("target: \"agent_gossip::") else {
                continue;
            };
            let Some((subsystem, _)) = rest.1.split_once('"') else {
                continue;
            };
            let target = format!("agent_gossip::{subsystem}");
            if !pins.contains(&format!("{target}=")) {
                unpinned.push(format!("  {}:{} — {target}", file.display(), index + 1));
            }
        }
    }

    assert!(
        unpinned.is_empty(),
        "these targets are emitted but not pinned in APP_LOG_PINS, so an \
         optimized build drops them silently:\n{}\n\ncurrent pins: {pins}",
        unpinned.join("\n")
    );
}
