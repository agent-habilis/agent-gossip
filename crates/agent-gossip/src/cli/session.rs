use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use futures_util::future::join_all;
use habilis_network::protocol::Nickname;
use habilis_network::runtime::state_file::{read_identity, read_session_entry};
use habilis_network::runtime::{ipc, state_file};
use habilis_network::util::process;

use super::args::{BellCheckOpts, LeaveOpts, SessionOpts};
use super::bell;
use super::discovery::Discovery;
use super::target::Target;
use crate::a2a::ipc_command::IpcCommand::Info;
use crate::runtime_base;

/// Walk every state file under [`runtime_base`](fn@runtime_base) and resolve it to a live
/// daemon or clean it up. Covers both per-mesh files
/// (`<prefix>/<nick>.state.json`) and the CLI-fallback location the skills
/// use (`sessions/<ppid>.json`) — any first-level `*.json`.
///
/// A state file outside the runtime base is found through its daemon: the socket is always in the
/// base, and the daemon's `Info` reply (in `owners`) names the file.
fn discover(owners: &[Owner]) -> Discovery {
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
                owner_pid: entry.owner_pid,
                pubkey: read_identity(&path).pubkey,
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
    for owner in owners {
        let Some(path) = &owner.state_file else {
            continue;
        };
        let walked = live
            .iter()
            .any(|target| target.pid == owner.pid && target.mesh == owner.gossip);
        if walked {
            continue;
        }
        live.extend(reported_target(owner, path));
    }
    Discovery { live, cleaned }
}

/// The target that a daemon's own reply points at, or `None` when the file does not agree with the
/// reply. It never removes a file: the reply names the path, and a path that a reply names is not
/// ours to delete.
fn reported_target(owner: &Owner, path: &Path) -> Option<Target> {
    let entry = read_session_entry(path)?;
    let wrong_nickname = entry
        .nickname
        .as_deref()
        .is_some_and(|nickname| nickname != owner.nickname);
    if entry.pid != Some(owner.pid)
        || entry.mesh.as_deref() != Some(owner.gossip.as_str())
        || wrong_nickname
    {
        return None;
    }
    Some(Target {
        owner_pid: entry.owner_pid,
        pubkey: read_identity(path).pubkey,
        path: path.to_path_buf(),
        mesh: owner.gossip.clone(),
        name: entry.name,
        nickname: entry.nickname,
        topic: entry.topic,
        pid: owner.pid,
    })
}

/// Whether the session anchored at `anchor` owns this daemon: the daemon is
/// its descendant, or the daemon named the anchor, or one of its descendants,
/// as its owner.
fn owned_by(target: &Target, anchor: u32) -> bool {
    process::ancestry_contains(target.pid, anchor)
        || target
            .owner_pid
            .is_some_and(|owner| owner == anchor || process::ancestry_contains(owner, anchor))
}

/// Resolve a live session's mesh id from its nickname — for commands that need
/// the session's runtime dir without retyping the mesh id (e.g. `a2a fetch`
/// landing a blob under `<nick>.recv`). `None` if no live session claims it.
pub(crate) async fn mesh_for_nickname(nickname: &str) -> Option<String> {
    discover(&live_owners().await.answered)
        .live
        .into_iter()
        .find(|target| target.nickname.as_deref() == Some(nickname))
        .map(|target| target.mesh)
}

fn state_file_paths() -> Vec<PathBuf> {
    let Ok(mesh_dirs) = std::fs::read_dir(runtime_base()) else {
        return Vec::new();
    };
    let entries = mesh_dirs.flatten().map(|dir_entry| dir_entry.path());
    let dirs = entries.filter(|path| path.is_dir());
    let files = dirs.flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten());
    let paths = files.map(|file_entry| file_entry.path());
    paths
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

/// Split the live daemons into the calling session's own vs everyone
/// else's. `is_owned` is injected so the split is testable without real
/// processes; the real caller passes an ancestry check against
/// `session_pid`.
fn split_owned(live: Vec<Target>, is_owned: impl Fn(&Target) -> bool) -> (Vec<Target>, usize) {
    let (owned, others): (Vec<Target>, Vec<Target>) =
        live.into_iter().partition(|target| is_owned(target));
    let other_sessions = others.len();
    (owned, other_sessions)
}

/// Select by explicit mesh id (full or prefix) and optional nickname — no
/// ownership check: naming the mesh is the intent.
fn select_explicit(live: Vec<Target>, mesh: &str, nickname: Option<&str>) -> Vec<Target> {
    let in_mesh = live
        .into_iter()
        .filter(|target| target.mesh.starts_with(mesh));
    in_mesh
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
    if let Some(pubkey) = &target.pubkey {
        obj.insert("pubkey".into(), pubkey.clone().into());
    }
    if let Some(topic) = &target.topic {
        obj.insert("topic".into(), topic.clone().into());
    }
    obj.insert("pid".into(), target.pid.into());
    serde_json::Value::Object(obj)
}

/// What a live daemon answered when asked who it is: its mesh, its nickname,
/// and — the part no state file can be trusted for — its own pid. `state_file` is the absolute path
/// it was started with, or `None` for the default under the runtime base (and for a daemon that
/// predates the field).
struct Owner {
    gossip: String,
    nickname: String,
    pid: u32,
    state_file: Option<PathBuf>,
}

/// A stopped daemon (`SIGSTOP`) still accepts the connection and never answers, and the engine's
/// client has no timeout of its own. Without this bound the Stop hook would wait on it for ever.
const OWNER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

fn parse_owner(response: &str) -> Option<Owner> {
    let value: serde_json::Value = serde_json::from_str(response).ok()?;
    let gossip = value["gossip"].as_str()?.to_owned();
    let nickname = value["nickname"].as_str()?.to_owned();
    let pid = u32::try_from(value["pid"].as_u64()?).ok()?;
    let state_file = value["state_file"]
        .as_str()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    Some(Owner {
        gossip,
        nickname,
        pid,
        state_file,
    })
}

/// How one daemon socket answered the `Info` probe.
enum Probe {
    Answered(Owner),
    /// No answer in time. The daemon can be alive: stopped, or slow under load.
    TimedOut(PathBuf),
    /// A refused connection or an unreadable reply: nothing serves there.
    Dead,
}

async fn probe(path: PathBuf) -> Probe {
    match tokio::time::timeout(OWNER_PROBE_TIMEOUT, ipc::send_to_path(&path, &Info)).await {
        Err(_) => Probe::TimedOut(path),
        Ok(Err(_)) => Probe::Dead,
        Ok(Ok(reply)) => parse_owner(&reply).map_or(Probe::Dead, Probe::Answered),
    }
}

/// What the daemon sockets of this machine answered.
struct Owners {
    answered: Vec<Owner>,
    /// The sockets that gave no answer in time.
    silent: Vec<PathBuf>,
}

impl Owners {
    /// Whether the probe of this target's own socket timed out. Such a daemon can be alive, and
    /// a timeout says nothing about it, so nobody removes its file or reports it gone.
    fn unknown(&self, target: &Target) -> bool {
        let Some(nickname) = &target.nickname else {
            return !self.silent.is_empty();
        };
        let dir = habilis_network::util::mesh_runtime_dir(&runtime_base(), &target.mesh);
        self.silent
            .contains(&dir.join(format!("{nickname}.ipc.sock")))
    }
}

/// Ask every daemon socket on this machine to identify itself.
///
/// Best-effort by design: a stale socket left by a `SIGKILL`ed daemon simply
/// fails to answer and is skipped, which is exactly the case this is here to
/// tell apart from a live one.
async fn live_owners() -> Owners {
    let base = runtime_base();
    let probes = ipc::active_socket_paths(&base).map(probe);
    let mut owners = Owners {
        answered: Vec::new(),
        silent: Vec::new(),
    };
    for result in join_all(probes).await {
        match result {
            Probe::Answered(owner) => owners.answered.push(owner),
            Probe::TimedOut(path) => owners.silent.push(path),
            Probe::Dead => {}
        }
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

/// The live daemon already writing `path`, described for an error message, or
/// `None` if the path is free.
///
/// A state file has exactly one writer: the daemon replaces it wholesale on
/// every heartbeat. Two daemons pointed at one path therefore take turns
/// owning it, and everything that resolves identity from that file — the bell,
/// `ready`, `session`, `leave` — follows whichever wrote last, so which agent
/// is deaf flips per heartbeat with nothing to indicate it. The skills derive
/// the path from the agent's parent pid, so two sessions under one parent
/// collide by construction rather than by accident.
///
/// A leftover file from a `SIGKILL`ed daemon is not a collision: nothing
/// answers for it, so the path is free and the new daemon overwrites it.
pub(crate) async fn live_owner_of(path: &Path) -> Option<String> {
    let entry = read_session_entry(path)?;
    let (mesh, pid) = (entry.mesh?, entry.pid?);
    let nickname = entry.nickname.clone();
    let target = Target {
        path: path.to_path_buf(),
        mesh: mesh.clone(),
        name: entry.name,
        nickname: nickname.clone(),
        pubkey: None,
        topic: entry.topic,
        pid,
        owner_pid: entry.owner_pid,
    };
    let owners = live_owners().await;
    if !confirm_owner(&owners.answered, &target) && !owners.unknown(&target) {
        return None;
    }
    let who = nickname.map_or_else(
        || format!("pid {pid}"),
        |nick| format!("<{nick}> (pid {pid})"),
    );
    Some(format!("{who} on gossip {mesh}"))
}

pub(crate) async fn leave(options: LeaveOpts) -> Result<()> {
    let LeaveOpts {
        gossip,
        gossip_flag,
        nickname,
        session_pid,
        confirm_timeout_secs,
        legacy_output: _,
    } = options;
    let mesh = gossip.or(gossip_flag);
    let owners = live_owners().await;
    let Discovery { live, cleaned } = discover(&owners.answered);
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
        split_owned(live, |target| owned_by(target, anchor))
    };

    let mut left = Vec::new();
    for target in &matched {
        // Confirm the pid still belongs to *this* session before signalling.
        // `discover` accepts any live `agent-gossip` process, which is enough
        // to list a session but not to kill one: a SIGKILLed daemon leaves its
        // state file behind, and once the OS reissues that pid to another
        // daemon, the file points at a stranger. Signalling on the file's word
        // alone terminates that stranger's gossip.
        if !confirm_owner(&owners.answered, target) {
            // A probe that timed out says nothing: a stopped or slow daemon is alive, and the
            // file is the one it writes.
            if !owners.unknown(target) {
                let _ = std::fs::remove_file(&target.path);
            }
            left.push((target.clone(), false));
            continue;
        }
        // A failed SIGTERM (EPERM/vanished pid) is reported per-entry, not a
        // hard error: the other matches must still be signalled.
        let signalled = process::terminate(target.pid).is_ok();
        left.push((target.clone(), signalled));
    }
    // `confirmed` has to mean the daemon is gone, not that its file is. It
    // removes that file early in shutdown and then keeps serving — still in the
    // mesh, still answering its socket — for another half-second, so waiting on
    // the file alone reported a departure that had not happened. A caller that
    // took it at its word and started a replacement got two daemons on one
    // identity, the new one announcing `joined` before the old one broadcast
    // `left`. Requiring silence on the socket too makes the report agree with
    // the predicate `run_session` refuses to start against.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(confirm_timeout_secs);
    let mut outcomes: Vec<(Target, bool, bool)> = left
        .into_iter()
        .map(|(target, signalled)| (target, signalled, false))
        .collect();
    loop {
        let still_serving = live_owners().await;
        let mut waiting = false;
        for (target, signalled, gone) in &mut outcomes {
            if !*signalled || *gone {
                continue;
            }
            if target.path.exists()
                || confirm_owner(&still_serving.answered, target)
                || still_serving.unknown(target)
            {
                waiting = true;
            } else {
                *gone = true;
            }
        }
        if !waiting || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let left_json: Vec<serde_json::Value> = outcomes
        .iter()
        .map(|(target, _, gone)| {
            let mut obj = target_json(target);
            obj["confirmed"] = (*gone).into();
            obj
        })
        .collect();
    let report = serde_json::json!({
        "ok": true,
        "left": left_json,
        "other_sessions": other_sessions,
        "cleaned": cleaned,
    });
    println!("{report}");
    Ok(())
}

pub(crate) async fn session(options: &SessionOpts) {
    let SessionOpts {
        session_pid,
        legacy_output: _,
    } = options;
    let Discovery { live, cleaned } = discover(&live_owners().await.answered);
    let (owned, other_sessions) = owned_sessions(live, *session_pid);

    let sessions: Vec<_> = owned
        .iter()
        .map(|target| {
            let mut json = target_json(target);
            json["bell"] = target
                .nickname
                .as_deref()
                .is_some_and(|nickname| bell::is_armed(&target.mesh, nickname))
                .into();
            json
        })
        .collect();
    let report = serde_json::json!({
        "ok": true,
        "sessions": sessions,
        "other_sessions": other_sessions,
        "cleaned": cleaned,
    });
    println!("{report}");
}

fn owned_sessions(live: Vec<Target>, session_pid: Option<u32>) -> (Vec<Target>, usize) {
    let anchor = session_pid.unwrap_or_else(default_session_pid);
    split_owned(live, |target| owned_by(target, anchor))
}

/// The sessions that are serving but have no bell armed. A session still
/// starting up is skipped: its first bell waits for `ready` before it can
/// take the lock.
fn unarmed(
    owned: &[Target],
    is_ready: impl Fn(&Target) -> bool,
    is_armed: impl Fn(&str, &str) -> bool,
) -> Vec<&str> {
    owned
        .iter()
        .filter_map(|target| {
            let nickname = target.nickname.as_deref()?;
            (!is_armed(&target.mesh, nickname) && is_ready(target)).then_some(nickname)
        })
        .collect()
}

fn is_ready(target: &Target) -> bool {
    state_file::read_snapshot(&target.path)
        .ok()
        .flatten()
        .is_some_and(|snapshot| {
            snapshot.ready && super::ready_state_file::ready_is_fresh(snapshot.last_updated)
        })
}

/// The Claude Code Stop hook's probe. It answers in the hook's JSON protocol
/// and returns whether it blocked, so the caller exits 3 and a shell
/// `bell-check && echo armed` tells the truth. The hook wraps it in `|| true`:
/// a missing binary, or one too old to know this command, never blocks a
/// turn, and Claude Code still reads the JSON.
pub(crate) async fn bell_check(options: &BellCheckOpts) -> bool {
    let &BellCheckOpts {
        session_pid,
        grace_ms,
    } = options;
    let (owned, _) = owned_sessions(discover(&live_owners().await.answered).live, session_pid);
    // A bell re-armed in the same breath as the turn ends may not hold its
    // lock yet.
    let deadline = std::time::Instant::now() + Duration::from_millis(grace_ms);
    loop {
        let missing = unarmed(&owned, is_ready, bell::is_armed);
        if missing.is_empty() {
            return false;
        }
        if std::time::Instant::now() >= deadline {
            let reason = format!(
                "gossip bell not armed for {}: run the Receive loop (poll + re-arm) before ending the turn",
                missing.join(", ")
            );
            println!(
                "{}",
                serde_json::json!({ "decision": "block", "reason": reason })
            );
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use habilis_network::util::process;

    use super::{
        Owner, Owners, Target, confirm_owner, owned_by, parse_owner, reported_target,
        select_explicit, split_owned, unarmed,
    };

    fn target(mesh: &str, nickname: &str, pid: u32) -> Target {
        Target {
            path: PathBuf::from("/nonexistent"),
            mesh: mesh.to_owned(),
            name: Some("cool-team".to_owned()),
            nickname: Some(nickname.to_owned()),
            pubkey: None,
            topic: None,
            pid,
            owner_pid: None,
        }
    }

    #[test]
    fn unarmed_skips_unready_and_armed_sessions() {
        let mut anonymous = target("m4", "x", 4);
        anonymous.nickname = None;
        let owned = [
            target("m1", "armed", 1),
            target("m2", "starting", 2),
            target("m3", "deaf", 3),
            anonymous,
        ];
        let missing = unarmed(
            &owned,
            |target| target.pid != 2,
            |_, nickname| nickname == "armed",
        );
        assert_eq!(missing, ["deaf"], "only the deaf session is missing a bell");
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
            state_file: None,
        }];

        // The stale entry: right pid, wrong gossip. Nobody answers for `aaaa`.
        assert!(
            !confirm_owner(&owners, &target("aaaa", "one-two", 10)),
            "a live daemon on another gossip must not stand in for this one"
        );
        // The daemon that does answer for its own gossip is confirmed.
        assert!(
            confirm_owner(&owners, &target("bbbb", "three-four", 10)),
            "a session owned by the caller is confirmed"
        );
        // Right gossip, but the pid moved on: also not ours to signal.
        assert!(
            !confirm_owner(&owners, &target("bbbb", "three-four", 11)),
            "a matching gossip at a different pid is a different process"
        );
        // A nickname that disagrees rules it out even with mesh and pid equal,
        // which is what separates two daemons sharing one state-file path.
        assert!(
            !confirm_owner(&owners, &target("bbbb", "someone-else", 10)),
            "a session of another owner is not confirmed"
        );
    }

    #[test]
    fn owned_by_follows_the_recorded_owner() {
        let own = std::process::id();
        let parent = process::parent_of(own).expect("test process has a parent");
        let mut detached = target("aaaa", "one-two", u32::MAX);
        assert!(
            !owned_by(&detached, own),
            "a detached session was never claimed, so no pid can own it"
        );
        detached.owner_pid = Some(own);
        assert!(owned_by(&detached, own), "the owner itself");
        assert!(owned_by(&detached, parent), "an ancestor of the owner");
    }

    #[test]
    fn split_owned_partitions_by_pid() {
        let live = vec![
            target("aaaa", "one-two", 10),
            target("bbbb", "three-four", 20),
        ];
        let (owned, other_sessions) = split_owned(live, |target| target.pid == 10);
        assert_eq!(owned.len(), 1, "one live target is owned by this session");
        assert_eq!(owned[0].pid, 10, "the owned target is the one with pid 10");
        assert_eq!(other_sessions, 1, "the other session is counted separately");
    }

    #[test]
    fn select_explicit_matches_full_id_and_prefix() {
        let live = vec![
            target("aaaabbbbccccdddd1111", "one-two", 10),
            target("zzzzyyyyxxxxwwww2222", "three-four", 20),
        ];
        let full = select_explicit(live.clone(), "aaaabbbbccccdddd1111", None);
        assert_eq!(full.len(), 1, "the full nickname selects one target");
        assert_eq!(full[0].pid, 10, "the full selection is the pid 10 target");
        let prefix = select_explicit(live, "aaaabbbbccccddd", None);
        assert_eq!(prefix.len(), 1, "a nickname prefix selects one target");
        assert_eq!(
            prefix[0].pid, 10,
            "the prefix selection is the pid 10 target"
        );
    }

    #[test]
    fn select_explicit_narrows_by_nickname() {
        let live = vec![
            target("aaaa", "one-two", 10),
            target("aaaa", "three-four", 20),
        ];
        let matched = select_explicit(live, "aaaa", Some("three-four"));
        assert_eq!(matched.len(), 1, "a nickname matches its own target");
        assert_eq!(matched[0].pid, 20, "the matched target is pid 20");
    }

    #[test]
    fn select_explicit_no_match_is_empty() {
        let live = vec![target("aaaa", "one-two", 10)];
        assert!(
            select_explicit(live, "none", None).is_empty(),
            "an unknown mesh selects nothing"
        );
    }

    #[test]
    fn an_info_reply_names_a_state_file_outside_the_runtime_base() {
        let reply = r#"{"ok":true,"gossip":"aaaa","name":"n","nickname":"one-two","peer_count":1,"pid":10,"state_file":"/srv/x/mac.json"}"#;
        let owner = parse_owner(reply).expect("a complete reply parses");
        assert_eq!(
            owner.state_file,
            Some(PathBuf::from("/srv/x/mac.json")),
            "an absolute path in the reply is kept as the daemon wrote it"
        );
    }

    #[test]
    fn an_info_reply_without_a_usable_state_file_names_none() {
        let replies = [
            // A daemon older than the field.
            r#"{"gossip":"aaaa","nickname":"one-two","pid":10}"#,
            // The default state file, in the runtime base.
            r#"{"gossip":"aaaa","nickname":"one-two","pid":10,"state_file":null}"#,
            // A relative path names no file for a caller in another directory.
            r#"{"gossip":"aaaa","nickname":"one-two","pid":10,"state_file":"mac.json"}"#,
        ];
        for reply in replies {
            let owner = parse_owner(reply).expect("the identity fields are all there");
            assert!(
                owner.state_file.is_none(),
                "{reply}: an older daemon, the default file and a relative path give no usable path"
            );
        }
    }

    #[test]
    fn a_reported_file_that_disagrees_with_the_reply_is_ignored_and_kept() {
        let dir = std::env::temp_dir().join(format!("reported-target-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        let path = dir.join("other.json");
        let owner = Owner {
            gossip: "aaaa".to_owned(),
            nickname: "one-two".to_owned(),
            pid: 10,
            state_file: Some(path.clone()),
        };

        // The file belongs to another daemon: right file, wrong pid and gossip.
        std::fs::write(
            &path,
            r#"{"gossip":"bbbb","pid":11,"nickname":"three-four"}"#,
        )
        .expect("the temp dir is writable");
        assert!(
            reported_target(&owner, &path).is_none(),
            "a file of another daemon is not this daemon's"
        );
        assert!(path.exists(), "a path that a reply names is never deleted");

        std::fs::write(
            &path,
            r#"{"gossip":"aaaa","pid":10,"nickname":"one-two","owner_pid":5}"#,
        )
        .expect("the temp dir is writable");
        let found = reported_target(&owner, &path).expect("the file agrees with the reply");
        assert_eq!(found.owner_pid, Some(5), "the file keeps its owner pid");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reported_file_of_another_nickname_is_ignored() {
        let dir = std::env::temp_dir().join(format!("reported-nick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        let path = dir.join("same-pid.json");
        std::fs::write(
            &path,
            r#"{"gossip":"aaaa","pid":10,"nickname":"someone-else"}"#,
        )
        .expect("the temp dir is writable");
        let owner = Owner {
            gossip: "aaaa".to_owned(),
            nickname: "one-two".to_owned(),
            pid: 10,
            state_file: Some(path.clone()),
        };
        assert!(
            reported_target(&owner, &path).is_none(),
            "confirm_owner would refuse this file, so discovery must not offer it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_silent_socket_makes_only_its_own_daemon_unknown() {
        let mesh = "aaaabbbbccccdddd1111";
        let socket = habilis_network::util::mesh_runtime_dir(&crate::runtime_base(), mesh)
            .join("one-two.ipc.sock");
        let owners = Owners {
            answered: Vec::new(),
            silent: vec![socket],
        };
        assert!(
            owners.unknown(&target(mesh, "one-two", 10)),
            "the daemon behind the silent socket may be alive"
        );
        assert!(
            !owners.unknown(&target(mesh, "three-four", 20)),
            "a silent socket of another member says nothing about this one"
        );
        assert!(
            !owners.unknown(&target("zzzzyyyyxxxxwwww2222", "one-two", 10)),
            "the same nickname on another gossip is another socket"
        );
    }
}
