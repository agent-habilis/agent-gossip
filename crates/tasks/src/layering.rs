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
    Rule {
        needle: "agent-gossip",
        why: "the engine naming a consumer. It has three (`agent-gossip`, `examples/mesh-pipe`, `agent-habilis-mesh-ffi`) and must know none of them: take the product name as a parameter (`util::runtime_base`), or return a typed error the consumer renders (`resolver::JoinTargetError`, `transport::ipc::NoDaemon`)",
    },
];

/// The rules run over the engine's sources only. Two things that *used* to be
/// carved out of them, and how each was removed rather than exempted:
///
/// - The `/tmp/agent-gossip-<uid>` runtime base is a live filesystem contract —
///   `skills/shared/daemon-session.md` hardcodes
///   `/tmp/agent-gossip-$(id -u)/sessions/${PPID}.json`, so the bytes cannot
///   change. `util::runtime_base` now takes the product name as an *argument*
///   and the consumer supplies it, which keeps the path identical while leaving
///   the engine ignorant of it. A parameter, not a registered global:
///   `runtime_base`'s determinism requirement means a registration missed on one
///   entry path would silently relocate the base for exactly the callers that
///   have to agree.
/// - `transport::ipc`'s "start one with `agent-gossip create`" text oriented a
///   human, and still does — but it is composed in `cli::ipc`, off the engine's
///   typed `NoDaemon`. Same sentence, named where the commands live.
const GUARDED: &str = "crates/agent-habilis-mesh/src";

/// The committed inventory of the engine's exported items, relative to the repo
/// root. Regenerate with `cargo task layering --bless`.
const SNAPSHOT: &str = "crates/agent-habilis-mesh/public-surface.txt";

/// Every `pub` item the engine exposes, one per line, sorted.
///
/// Deliberately textual rather than rustdoc-derived: rustdoc JSON is precise but
/// needs a nightly toolchain in this crate, and the failure this guards against
/// — a `pub` slipping back in unnoticed — is visible in the source line itself.
fn public_surface(files: &[PathBuf]) -> Vec<String> {
    let mut items = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let shown = file
            .strip_prefix(repo_root())
            .unwrap_or(file)
            .display()
            .to_string();
        for line in text.lines() {
            let trimmed = line.trim();
            // `pub ` only — `pub(crate)` / `pub(super)` are not surface.
            if !trimmed.starts_with("pub ") {
                continue;
            }
            if !matches!(
                trimmed.split_whitespace().nth(1),
                Some(
                    "fn" | "struct"
                        | "enum"
                        | "trait"
                        | "type"
                        | "const"
                        | "mod"
                        | "use"
                        | "async"
                        | "unsafe"
                )
            ) {
                continue;
            }
            items.push(format!("{shown}\t{}", trimmed.trim_end_matches('{').trim()));
        }
    }
    items.sort();
    items
}

fn check_surface(files: &[PathBuf], bless: bool) -> TaskOutcome {
    let path = repo_root().join(SNAPSHOT);
    let current = public_surface(files).join("\n") + "\n";
    if bless {
        std::fs::write(&path, &current)?;
        let count = current.lines().count();
        output::status("Blessed", &format!("public surface ({count} items)"));
        return Ok(());
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed == current {
        let count = current.lines().count();
        output::status("Checked", &format!("public surface ({count} items)"));
        return Ok(());
    }
    let was: Vec<&str> = committed.lines().collect();
    let now: Vec<&str> = current.lines().collect();
    let added: Vec<&&str> = now.iter().filter(|line| !was.contains(line)).collect();
    let removed: Vec<&&str> = was.iter().filter(|line| !now.contains(line)).collect();
    let mut message = String::from(
        "the engine's public surface changed.\n\n\
         Widening it is a design decision, not a side effect: every item here is\n\
         something a consumer can reach into. If the change is intended, run\n\
         `cargo task layering --bless` and commit the snapshot with it.\n\n",
    );
    for line in &added {
        let _ = writeln!(message, "  + {line}");
    }
    for line in &removed {
        let _ = writeln!(message, "  - {line}");
    }
    Err(message.into())
}

pub(crate) fn run(bless: bool) -> TaskOutcome {
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
        return check_surface(&files, bless);
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
