use std::path::PathBuf;

use anyhow::Result;

use crate::runtime_base;
use fofoca::protocol::Nickname;
use fofoca::runtime::state_file::read_session_entry;
use fofoca::util::process;

use super::args::{LeaveOpts, SessionOpts};

/// One live daemon on this machine, resolved from its state file. `mesh`
/// and `pid` are required to act on an entry; `name`/`nickname`/`topic` are
/// carried for reporting only.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    path: PathBuf,
    mesh: String,
    name: Option<String>,
    nickname: Option<String>,
    /// The raw topic string (topic gossips only) — what the leave report
    /// echoes instead of the lossy derived `name`.
    topic: Option<String>,
    pid: u32,
}

pub(crate) struct Discovery {
    live: Vec<Target>,
    /// State files whose recorded pid is gone (or reused by a non-`agent-gossip`
    /// process) — leftovers of a SIGKILL/power-loss that the daemon never
    /// got to remove itself. Deleted during discovery so they stop
    /// rendering as ghost sessions (e.g. a statusline pill).
    cleaned: usize,
}

/// Walk every state file under [`runtime_base`] and resolve it to a live
/// daemon or clean it up. Covers both per-mesh files
/// (`<prefix>/<nick>.state.json`) and the CLI-fallback location the skills
/// use (`sessions/<ppid>.json`) — any first-level `*.json`.
fn discover() -> Discovery {
    let mut live = Vec::new();
    let mut cleaned = 0;
    for path in state_file_paths() {
        let Some(entry) = read_session_entry(&path) else {
            continue;
        };
        let (Some(mesh), Some(pid)) = (entry.mesh, entry.pid) else {
            // A pre-`pid` (old-binary) file: no way to verify or signal its
            // daemon, so it is neither actionable nor safely removable.
            continue;
        };
        if process::is_alive(pid) && process::comm_of(pid).as_deref() == Some("agent-gossip") {
            live.push(Target {
                path,
                mesh,
                name: entry.name,
                nickname: entry.nickname,
                topic: entry.topic,
                pid,
            });
        } else {
            let _ = std::fs::remove_file(&path);
            cleaned += 1;
        }
    }
    Discovery { live, cleaned }
}

/// Resolve a live session's mesh id from its nickname — for commands that need
/// the session's runtime dir without retyping the mesh id (e.g. `a2a fetch`
/// landing a blob under `<nick>.recv`). `None` if no live session claims it.
pub(crate) fn mesh_for_nickname(nickname: &str) -> Option<String> {
    discover()
        .live
        .into_iter()
        .find(|target| target.nickname.as_deref() == Some(nickname))
        .map(|target| target.mesh)
}

fn state_file_paths() -> Vec<PathBuf> {
    let Ok(mesh_dirs) = std::fs::read_dir(runtime_base()) else {
        return Vec::new();
    };
    mesh_dirs
        .flatten()
        .map(|dir_entry| dir_entry.path())
        .filter(|path| path.is_dir())
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|file_entry| file_entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

/// Split the live daemons into the calling session's own vs everyone
/// else's. `is_owned` is injected so the split is testable without real
/// processes; the real caller passes an ancestry check against
/// `session_pid`.
fn split_owned(live: Vec<Target>, is_owned: impl Fn(u32) -> bool) -> (Vec<Target>, usize) {
    let (owned, others): (Vec<Target>, Vec<Target>) =
        live.into_iter().partition(|target| is_owned(target.pid));
    let other_sessions = others.len();
    (owned, other_sessions)
}

/// Select by explicit mesh id (full or prefix) and optional nickname — no
/// ownership check: naming the mesh is the intent.
fn select_explicit(live: Vec<Target>, mesh: &str, nickname: Option<&str>) -> Vec<Target> {
    live.into_iter()
        .filter(|target| target.mesh.starts_with(mesh))
        .filter(|target| nickname.is_none_or(|wanted| target.nickname.as_deref() == Some(wanted)))
        .collect()
}

/// The default ownership anchor when `--session-pid` is not given: this
/// command's own parent. When an agent's Bash tool runs `agent-gossip leave`
/// directly (not via a wrapping shell script), that parent *is* the shell
/// whose parent is the agent — so skills pass `$PPID` explicitly instead of
/// relying on this.
fn default_session_pid() -> u32 {
    process::parent_of(std::process::id()).unwrap_or(1)
}

fn target_json(target: &Target) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("gossip".into(), target.mesh.clone().into());
    if let Some(name) = &target.name {
        obj.insert("name".into(), name.clone().into());
    }
    if let Some(nickname) = &target.nickname {
        obj.insert("nickname".into(), nickname.clone().into());
    }
    if let Some(topic) = &target.topic {
        obj.insert("topic".into(), topic.clone().into());
    }
    obj.insert("pid".into(), target.pid.into());
    serde_json::Value::Object(obj)
}

/// What a live daemon answered when asked who it is: its mesh, its nickname,
/// and — the part no state file can be trusted for — its own pid.
struct Owner {
    gossip: String,
    nickname: String,
    pid: u32,
}

/// Ask every daemon socket on this machine to identify itself.
///
/// Best-effort by design: a stale socket left by a `SIGKILL`ed daemon simply
/// fails to answer and is skipped, which is exactly the case this is here to
/// tell apart from a live one.
async fn live_owners() -> Vec<Owner> {
    let mut owners = Vec::new();
    for path in fofoca::runtime::ipc::active_socket_paths(&runtime_base()) {
        let Ok(response) =
            fofoca::runtime::ipc::send_to_path(&path, &crate::a2a::ipc::IpcCommand::Info).await
        else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&response) else {
            continue;
        };
        let (Some(gossip), Some(nickname), Some(pid)) = (
            value["gossip"].as_str(),
            value["nickname"].as_str(),
            value["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok()),
        ) else {
            continue;
        };
        owners.push(Owner {
            gossip: gossip.to_owned(),
            nickname: nickname.to_owned(),
            pid,
        });
    }
    owners
}

/// Does a live daemon actually answer for this target's mesh *at this pid*?
///
/// Both halves matter. Matching only the mesh would still signal the wrong
/// process when a stale file's pid has been reissued; matching only the pid
/// would signal a live daemon that has nothing to do with the mesh named. A
/// daemon predating this check answers without a `pid` and is treated as
/// unconfirmed, which costs a stale entry a cleanup rather than risking a
/// stranger's process.
fn confirm_owner(owners: &[Owner], target: &Target) -> bool {
    owners.iter().any(|owner| {
        owner.pid == target.pid
            && owner.gossip == target.mesh
            && target
                .nickname
                .as_ref()
                .is_none_or(|wanted| owner.nickname == *wanted)
    })
}

pub(crate) async fn leave(opts: LeaveOpts) -> Result<()> {
    let LeaveOpts {
        gossip: mesh,
        nickname,
        session_pid,
        confirm_timeout_secs,
        legacy_output: _,
    } = opts;
    let Discovery { live, cleaned } = discover();
    let (matched, other_sessions) = if let Some(mesh_id) = &mesh {
        (
            select_explicit(
                live,
                mesh_id.as_str(),
                nickname.as_ref().map(Nickname::as_str),
            ),
            0,
        )
    } else {
        let anchor = session_pid.unwrap_or_else(default_session_pid);
        split_owned(live, |pid| process::ancestry_contains(pid, anchor))
    };

    let owners = live_owners().await;
    let mut left = Vec::new();
    for target in &matched {
        // Confirm the pid still belongs to *this* session before signalling.
        // `discover` accepts any live `agent-gossip` process, which is enough
        // to list a session but not to kill one: a SIGKILLed daemon leaves its
        // state file behind, and once the OS reissues that pid to another
        // daemon, the file points at a stranger. Signalling on the file's word
        // alone terminates that stranger's gossip.
        if !confirm_owner(&owners, target) {
            let _ = std::fs::remove_file(&target.path);
            left.push((target.clone(), false));
            continue;
        }
        // A failed SIGTERM (EPERM/vanished pid) is reported per-entry, not a
        // hard error: the other matches must still be signalled.
        let signalled = process::terminate(target.pid).is_ok();
        left.push((target.clone(), signalled));
    }
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(confirm_timeout_secs);
    let mut confirmed: Vec<(Target, bool)> = Vec::new();
    for (target, signalled) in left {
        let mut gone = false;
        while signalled && tokio::time::Instant::now() < deadline {
            if !target.path.exists() {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        confirmed.push((target, gone));
    }

    let report = serde_json::json!({
        "ok": true,
        "left": confirmed
            .iter()
            .map(|(target, gone)| {
                let mut obj = target_json(target);
                obj["confirmed"] = (*gone).into();
                obj
            })
            .collect::<Vec<_>>(),
        "other_sessions": other_sessions,
        "cleaned": cleaned,
    });
    println!("{report}");
    Ok(())
}

#[expect(
    clippy::unused_async,
    reason = "keeps the dispatch arm uniform with every other subcommand"
)]
pub(crate) async fn session(opts: SessionOpts) -> Result<()> {
    let SessionOpts {
        session_pid,
        legacy_output: _,
    } = opts;
    let Discovery { live, cleaned } = discover();
    let anchor = session_pid.unwrap_or_else(default_session_pid);
    let (owned, other_sessions) = split_owned(live, |pid| process::ancestry_contains(pid, anchor));

    let report = serde_json::json!({
        "ok": true,
        "sessions": owned.iter().map(target_json).collect::<Vec<_>>(),
        "other_sessions": other_sessions,
        "cleaned": cleaned,
    });
    println!("{report}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Owner, Target, confirm_owner, select_explicit, split_owned};

    fn target(mesh: &str, nickname: &str, pid: u32) -> Target {
        Target {
            path: std::path::PathBuf::from("/nonexistent"),
            mesh: mesh.to_owned(),
            name: Some("cool-team".to_owned()),
            nickname: Some(nickname.to_owned()),
            topic: None,
            pid,
        }
    }

    /// The scenario that reissued pids create, and the reason `leave` cannot
    /// act on a state file's word alone.
    ///
    /// Daemon A (mesh `aaaa`, pid 10) dies by SIGKILL, so its file survives
    /// with pid 10 still written in it. The OS hands pid 10 to daemon B, on
    /// mesh `bbbb`. `discover` accepts the stale entry as live — the pid is
    /// alive and the process really is called `agent-gossip` — so `leave
    /// --gossip aaaa` used to SIGTERM daemon B, a gossip it was never asked
    /// about, and then report `confirmed: false` because A's orphan file was
    /// never going to disappear.
    #[test]
    fn a_reissued_pid_is_not_confirmed_as_the_owner() {
        let owners = vec![Owner {
            gossip: "bbbb".to_owned(),
            nickname: "three-four".to_owned(),
            pid: 10,
        }];

        // The stale entry: right pid, wrong gossip. Nobody answers for `aaaa`.
        assert!(
            !confirm_owner(&owners, &target("aaaa", "one-two", 10)),
            "a live daemon on another gossip must not stand in for this one"
        );
        // The daemon that does answer for its own gossip is confirmed.
        assert!(confirm_owner(&owners, &target("bbbb", "three-four", 10)));
        // Right gossip, but the pid moved on: also not ours to signal.
        assert!(
            !confirm_owner(&owners, &target("bbbb", "three-four", 11)),
            "a matching gossip at a different pid is a different process"
        );
        // A nickname that disagrees rules it out even with mesh and pid equal,
        // which is what separates two daemons sharing one state-file path.
        assert!(!confirm_owner(&owners, &target("bbbb", "someone-else", 10)));
    }

    #[test]
    fn split_owned_partitions_by_pid() {
        let live = vec![
            target("aaaa", "one-two", 10),
            target("bbbb", "three-four", 20),
        ];
        let (owned, other_sessions) = split_owned(live, |pid| pid == 10);
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].pid, 10);
        assert_eq!(other_sessions, 1);
    }

    #[test]
    fn select_explicit_matches_full_id_and_prefix() {
        let live = vec![
            target("aaaabbbbccccdddd1111", "one-two", 10),
            target("zzzzyyyyxxxxwwww2222", "three-four", 20),
        ];
        let full = select_explicit(live.clone(), "aaaabbbbccccdddd1111", None);
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].pid, 10);
        let prefix = select_explicit(live, "aaaabbbbccccddd", None);
        assert_eq!(prefix.len(), 1);
        assert_eq!(prefix[0].pid, 10);
    }

    #[test]
    fn select_explicit_narrows_by_nickname() {
        let live = vec![
            target("aaaa", "one-two", 10),
            target("aaaa", "three-four", 20),
        ];
        let matched = select_explicit(live, "aaaa", Some("three-four"));
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].pid, 20);
    }

    #[test]
    fn select_explicit_no_match_is_empty() {
        let live = vec![target("aaaa", "one-two", 10)];
        assert!(select_explicit(live, "none", None).is_empty());
    }
}
