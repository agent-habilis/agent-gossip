use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::TaskOutcome;
use crate::util::{output, repo_root};

/// One rule: a substring the engine crate must not contain, and why.
struct Rule {
    needle: &'static str,
    why: &'static str,
}

/// The engine may know it carries *an application payload*. It may not know
/// which application, what that payload means, or what identity the product
/// stamps on the wire. A leak of any of these makes the payload-generic claim
/// (and `examples/mesh-pipe`, which exists to prove it) untrue.
///
/// Substrings, not identifiers: a doc comment framing the engine as sitting
/// "under the A2A layer" is exactly the leak this guards, and prose leaks the
/// concept long before a type name does.
const RULES: &[Rule] = &[
    Rule {
        needle: "a2a",
        why: "A2A is the application's data model; the engine routes opaque payloads",
    },
    Rule {
        needle: "agent_gossip::",
        why: "a crate-path/log-target naming the application; the engine's own is `agent_habilis_mesh::`",
    },
    Rule {
        needle: "b\"agent-gossip",
        why: "a branded byte-domain: ALPNs and derivation labels are mixed into signature and key transcripts, so the product name would put the app on the wire",
    },
];

/// Deliberately **not** forbidden: prose and paths that name the `agent-gossip`
/// CLI. Two reasons, both load-bearing:
///
/// - `util::mod`'s `/tmp/agent-gossip-<uid>` runtime base is a live filesystem
///   contract — `skills/shared/daemon-session.md` hardcodes
///   `/tmp/agent-gossip-$(id -u)/sessions/${PPID}.json`, so renaming it orphans
///   every running daemon's session file and breaks the skills.
/// - `transport::ipc`'s "start one with `agent-gossip create`" error text and
///   the doc comments citing CLI commands orient a human. Stripping the command
///   name to satisfy a lint would make both worse.
///
/// Neither puts A2A in the engine, and neither reaches the wire.
const GUARDED: &str = "crates/agent-habilis-mesh/src";

pub(crate) fn run() -> TaskOutcome {
    let root = repo_root().join(GUARDED);
    let mut violations = Vec::new();
    let mut files = Vec::new();
    collect_rs(&root, &mut files)?;
    files.sort();

    for file in &files {
        let text = std::fs::read_to_string(file)?;
        for (index, line) in text.lines().enumerate() {
            let lowered = line.to_ascii_lowercase();
            let Some(rule) = RULES.iter().find(|rule| lowered.contains(rule.needle)) else {
                continue;
            };
            let shown = file.strip_prefix(repo_root()).unwrap_or(file);
            violations.push(format!(
                "  {}:{}\n    {}\n    ^ names `{}` — {}",
                shown.display(),
                index + 1,
                line.trim(),
                rule.needle,
                rule.why
            ));
        }
    }

    if violations.is_empty() {
        output::status("Checked", &format!("layering ({} files)", files.len()));
        return Ok(());
    }

    let mut message = String::from(
        "the engine crate leaks the application layer.\n\n\
         `agent-habilis-mesh` is the payload-generic engine: it routes on a frame's\n\
         tag and addressee and never parses the body. Move the concept up into\n\
         `agent-gossip`, or reword it generically.\n\n",
    );
    for violation in &violations {
        let _ = writeln!(message, "{violation}");
    }
    let _ = write!(message, "\n{} violation(s)", violations.len());
    Err(message.into())
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rs(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    Ok(())
}
