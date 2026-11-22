use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use habilis_network::protocol::Nickname;
use habilis_network::runtime::state_file::{read_identity, read_session_entry};
use habilis_network::runtime::{
    DaemonRecord, DaemonState, read_record, registry, state_file, wait_daemon_lock,
};
use habilis_network::util::process;
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::args::{BellCheckOpts, LeaveOpts, SessionOpts};
use super::read_member::read_member;
use super::target::Target;
use super::{bell, leftovers};
use crate::runtime_base;

/// The daemons of the runtime base that run. The registry holds a lock file for each mesh and nickname
/// that was used, and a daemon that runs has a record in it whose pid and start name a live process.
/// No lock is taken and no daemon is asked: the record is the whole proof, so a stopped daemon is
/// listed, and a state file with no live record is not.
fn discover(base: &Path) -> Vec<Target> {
    let Ok(entries) = registry(base) else {
        return Vec::new();
    };
    let locks: Vec<PathBuf> = entries.flatten().collect();
    locks.iter().filter_map(|lock| target_of(lock)).collect()
}

/// The target behind a lock file, or `None` when no daemon runs for it: the record is not written yet,
/// or its process is gone. A record that says unknown is listed, because that daemon can run.
fn target_of(lock: &Path) -> Option<Target> {
    let record = read_record(lock).ok().flatten()?;
    match record.state() {
        DaemonState::Live | DaemonState::Unknown => Some(target_from(lock, record)),
        DaemonState::Gone => None,
    }
}

/// The target of a record, with the fields that its state file adds. A daemon that is not ready has no
/// state file yet, and its target keeps the fields of the record alone: nobody waits for the file.
fn target_from(lock: &Path, record: DaemonRecord) -> Target {
    let entry = read_session_entry(&record.state_file);
    let name = entry.as_ref().and_then(|entry| entry.name.clone());
    let topic = entry.as_ref().and_then(|entry| entry.topic.clone());
    let owner_pid = entry.and_then(|entry| entry.owner_pid);
    let pubkey = read_identity(&record.state_file).pubkey;
    let mesh = record.mesh.as_str().to_owned();
    let nickname = record.nickname.as_str().to_owned();
    Target {
        pubkey,
        name,
        topic,
        owner_pid,
        lock: lock.to_path_buf(),
        mesh,
        nickname,
        pid: record.pid,
        start: record.start,
        path: record.state_file,
    }
}

/// Whether the daemon of `anchor`'s session owns this daemon: the daemon is
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
pub(crate) fn mesh_for_nickname(nickname: &str) -> Option<String> {
    discover(&runtime_base())
        .into_iter()
        .find(|target| target.nickname == nickname)
        .map(|target| target.mesh)
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
        .filter(|target| nickname.is_none_or(|wanted| target.nickname == wanted))
        .collect()
}

/// The pid that names the calling session: `--session-pid`, or when it is not given this command's
/// own parent. When an agent's Bash tool runs `agent-gossip leave` directly (not via a wrapping shell
/// script), that parent *is* the shell whose parent is the agent — so skills pass `$PPID` explicitly
/// instead of relying on this.
///
/// Pid 0 and pid 1 name no session: init is an ancestor of every process, so a daemon is "owned" by
/// it, and a command that selected by it would stop the daemons of every session on the host. They
/// are refused as `--owner-pid` is, and so is a parent that is unknown or is init (an orphaned shell).
pub(super) fn session_anchor(session_pid: Option<u32>, parent: Option<u32>) -> Result<u32> {
    let Some(pid) = session_pid.or(parent) else {
        anyhow::bail!(
            "the parent process of this command is unknown, so no session is named: pass --session-pid"
        );
    };
    if pid <= 1 {
        anyhow::bail!(
            "session pid {pid} is init or invalid: it is an ancestor of every process, so it would select the daemons of every session. Pass the pid of the agent process"
        );
    }
    Ok(pid)
}

fn target_json(target: &Target) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("gossip".into(), target.mesh.clone().into());
    if let Some(name) = &target.name {
        obj.insert("name".into(), name.clone().into());
    }
    obj.insert("nickname".into(), target.nickname.clone().into());
    if let Some(pubkey) = &target.pubkey {
        obj.insert("pubkey".into(), pubkey.clone().into());
    }
    obj.insert("member".into(), read_member(&target.path));
    if let Some(topic) = &target.topic {
        obj.insert("topic".into(), topic.clone().into());
    }
    obj.insert("pid".into(), target.pid.into());
    serde_json::Value::Object(obj)
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
    let parent = process::parent_of(std::process::id());
    // A named pid that names no session is refused, whether or not GOSSIP leaves it unused.
    if let Some(pid) = session_pid {
        session_anchor(Some(pid), parent)?;
    }
    let base = runtime_base();
    let live = discover(&base);
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
        let anchor = session_anchor(session_pid, parent)?;
        split_owned(live, |target| owned_by(target, anchor))
    };

    // A failed SIGTERM (EPERM/vanished pid) is reported per entry, not a hard error: the other matches
    // must still be signalled.
    let signalled: Vec<(Target, bool)> = matched
        .into_iter()
        .map(|target| {
            let sent = runs_now(&target) && process::terminate(target.pid).is_ok();
            (target, sent)
        })
        .collect();
    // The waits start together and share 1 limit. A thread waits for the lock of each signalled daemon,
    // and the lock is free when that daemon has ended and removed its files.
    let deadline = Instant::now() + Duration::from_secs(confirm_timeout_secs);
    let mut waits: Vec<Option<oneshot::Receiver<bool>>> = Vec::new();
    for (target, sent) in &signalled {
        waits.push(sent.then(|| wait_for_stop(target.lock.clone())));
    }
    let mut left_json = Vec::new();
    for ((target, _), wait) in signalled.iter().zip(waits) {
        let confirmed = match wait {
            Some(wait) => stopped_before(wait, deadline).await,
            None => false,
        };
        let mut obj = target_json(target);
        obj["confirmed"] = confirmed.into();
        left_json.push(obj);
    }

    leftovers::sweep_logs(&base);

    let report = serde_json::json!({
        "ok": true,
        "left": left_json,
        "other_sessions": other_sessions,
    });
    println!("{report}");
    Ok(())
}

/// Whether the record of the target says, now, that its daemon runs. The pid is signalled on this word
/// alone, so the record is read again just before the signal, and the pids 0 and 1 are never signalled.
fn runs_now(target: &Target) -> bool {
    let record = read_record(&target.lock).ok().flatten();
    record.is_some_and(|record| {
        may_signal(
            (target.pid, target.start),
            (record.pid, record.start),
            record.state(),
        )
    })
}

/// The 3 conditions of a signal. The record names the process of the target: the pid and the start that
/// discovery read, because a new daemon that takes the pid of an ended one has another start. The pid is
/// not 0 or 1 (a signal to 0 goes to a whole process group, and 1 is init). And the daemon of the record
/// runs. The 2 pairs are `(pid, start)`.
fn may_signal(target: (u32, u64), record: (u32, u64), state: DaemonState) -> bool {
    record == target && record.0 > 1 && state == DaemonState::Live
}

/// Wait on a plain thread for the lock of a daemon, which is free when the daemon has ended. The thread is
/// not a task of the runtime: the runtime waits for its blocking tasks when it drops, and a daemon that
/// never ends would keep the process from exiting, while a plain thread dies with the process.
fn wait_for_stop(lock: PathBuf) -> oneshot::Receiver<bool> {
    let (sender, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        // The guard drops at the end of this statement, so a start that follows is never refused by it.
        let stopped = wait_daemon_lock(&lock).is_ok();
        let _ = sender.send(stopped);
    });
    receiver
}

async fn stopped_before(wait: oneshot::Receiver<bool>, deadline: Instant) -> bool {
    matches!(tokio::time::timeout_at(deadline, wait).await, Ok(Ok(true)))
}

pub(crate) fn session(options: &SessionOpts) -> Result<()> {
    let SessionOpts {
        session_pid,
        legacy_output: _,
    } = options;
    let anchor = session_anchor(*session_pid, process::parent_of(std::process::id()))?;
    let base = runtime_base();
    let live = discover(&base);
    leftovers::sweep_logs(&base);
    let (owned, other_sessions) = owned_sessions(live, anchor);

    let sessions: Vec<_> = owned
        .iter()
        .map(|target| {
            let mut json = target_json(target);
            json["bell"] = bell::is_armed(&target.mesh, &target.nickname).into();
            json
        })
        .collect();
    let report = serde_json::json!({
        "ok": true,
        "sessions": sessions,
        "other_sessions": other_sessions,
    });
    println!("{report}");
    Ok(())
}

fn owned_sessions(live: Vec<Target>, anchor: u32) -> (Vec<Target>, usize) {
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
    let deaf = owned
        .iter()
        .filter(|target| !is_armed(&target.mesh, &target.nickname) && is_ready(target));
    deaf.map(|target| target.nickname.as_str()).collect()
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
pub(crate) async fn bell_check(options: &BellCheckOpts) -> Result<bool> {
    let &BellCheckOpts {
        session_pid,
        grace_ms,
    } = options;
    let anchor = session_anchor(session_pid, process::parent_of(std::process::id()))?;
    let (owned, _) = owned_sessions(discover(&runtime_base()), anchor);
    // A bell re-armed in the same breath as the turn ends may not hold its
    // lock yet.
    let deadline = std::time::Instant::now() + Duration::from_millis(grace_ms);
    loop {
        let missing = unarmed(&owned, is_ready, bell::is_armed);
        if missing.is_empty() {
            return Ok(false);
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
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use habilis_network::runtime::DaemonState;
    use habilis_network::util::process;
    use habilis_network::util::process_start::ProcessStart;

    use super::{
        Target, discover, may_signal, owned_by, runs_now, select_explicit, session_anchor,
        split_owned, unarmed,
    };

    /// A mesh id that the engine accepts, for the records that these tests write.
    const MESH: &str = "3HUwLw2tgo15FvgqRBckue6iTFfSwuAXidiQuA6vrpoYZpSug11nR6g5rzaEsLjyLnD2wctm";

    fn target(mesh: &str, nickname: &str, pid: u32) -> Target {
        Target {
            path: PathBuf::from("/nonexistent"),
            lock: PathBuf::from("/nonexistent.lock"),
            mesh: mesh.to_owned(),
            name: Some("cool-team".to_owned()),
            nickname: nickname.to_owned(),
            pubkey: None,
            topic: None,
            pid,
            start: 0,
            owner_pid: None,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("locks")).expect("the temp dir is writable");
        base
    }

    /// The start that the system gives for this process, as the record of a daemon keeps it.
    fn own_start() -> u64 {
        match process::start_of(std::process::id()) {
            ProcessStart::Live(start) => start,
            ProcessStart::Gone | ProcessStart::Unknown(_) => {
                panic!("the test process runs, so its start can be read")
            }
        }
    }

    fn write_record(
        base: &Path,
        nickname: &str,
        process: (u32, u64),
        state_file: &Path,
    ) -> PathBuf {
        let (pid, start) = process;
        let lock = base
            .join("locks")
            .join(format!("3HUwLw2tgo15FvgqRBc-{nickname}.lock"));
        let line = serde_json::json!({
            "pid": pid,
            "start": start,
            "mesh": MESH,
            "nickname": nickname,
            "state_file": state_file,
        });
        std::fs::write(&lock, format!("{line}\n")).expect("the temp dir is writable");
        lock
    }

    #[test]
    fn unarmed_skips_unready_and_armed_sessions() {
        let owned = [
            target("m1", "armed", 1),
            target("m2", "starting", 2),
            target("m3", "deaf", 3),
        ];
        let missing = unarmed(
            &owned,
            |target| target.pid != 2,
            |_, nickname| nickname == "armed",
        );
        assert_eq!(missing, ["deaf"], "only the deaf session is missing a bell");
    }

    #[test]
    fn a_live_record_is_listed_with_the_fields_of_its_state_file() {
        let base = scratch("live");
        let state = base.join("one-two.state.json");
        std::fs::write(
            &state,
            r#"{"gossip":"x","pid":1,"name":"cool-team","nickname":"one-two","owner_pid":5}"#,
        )
        .expect("the temp dir is writable");
        write_record(&base, "one-two", (std::process::id(), own_start()), &state);

        let listed = discover(&base);

        assert_eq!(listed.len(), 1, "one daemon runs: {listed:?}");
        assert_eq!(listed[0].pid, std::process::id(), "the pid is the record's");
        assert_eq!(listed[0].mesh, MESH, "the mesh is the record's");
        assert_eq!(
            listed[0].name.as_deref(),
            Some("cool-team"),
            "the state file adds the name"
        );
        assert_eq!(
            listed[0].owner_pid,
            Some(5),
            "and the owner of a detached daemon"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_live_record_with_no_state_file_is_listed_with_the_fields_of_the_record() {
        let base = scratch("no-state");
        let state = base.join("not-there-yet.json");
        write_record(&base, "one-two", (std::process::id(), own_start()), &state);

        let listed = discover(&base);

        assert_eq!(
            listed.len(),
            1,
            "a daemon that is not ready yet is listed: {listed:?}"
        );
        assert_eq!(
            listed[0].nickname, "one-two",
            "the record names the nickname"
        );
        assert!(listed[0].name.is_none(), "nobody waits for the state file");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_gone_process_a_reused_pid_and_a_cut_record_list_nothing() {
        let base = scratch("gone");
        let state = base.join("x.json");
        let mut finished = Command::new("sleep")
            .arg("0")
            .spawn()
            .expect("sleep is on PATH on every unix host");
        let dead_pid = finished.id();
        finished.wait().expect("a finished child can be waited on");
        write_record(&base, "dead-one", (dead_pid, 1), &state);
        // The pid of a live process with another start: a later process that got the pid of the daemon.
        write_record(
            &base,
            "reused-pid",
            (std::process::id(), own_start() + 1),
            &state,
        );
        let locks = base.join("locks");
        std::fs::write(
            locks.join("3HUwLw2tgo15FvgqRBc-cut-one.lock"),
            r#"{"pid":1"#,
        )
        .expect("the temp dir is writable");
        std::fs::write(locks.join("3HUwLw2tgo15FvgqRBc-empty-one.lock"), "")
            .expect("the temp dir is writable");

        let listed = discover(&base);

        assert!(
            listed.is_empty(),
            "no daemon runs behind these files: {listed:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_folder_with_no_locks_lists_nothing() {
        let base = std::env::temp_dir().join(format!("session-nolocks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("the temp dir is writable");

        assert!(
            discover(&base).is_empty(),
            "a base that has no locks folder has no daemon"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_signal_goes_only_to_the_daemon_that_the_record_names_now() {
        let base = scratch("runs");
        let state = base.join("x.json");
        let own = std::process::id();
        let lock = write_record(&base, "one-two", (own, own_start()), &state);
        let mut live = target("m", "one-two", own);
        live.lock = lock;
        live.start = own_start();
        assert!(
            runs_now(&live),
            "the record names a live process with its start"
        );

        let mut other_pid = live.clone();
        other_pid.pid = own + 1;
        assert!(
            !runs_now(&other_pid),
            "a target whose pid is not the record's is not signalled"
        );

        write_record(&base, "one-two", (own, own_start() + 1), &state);
        assert!(
            !runs_now(&live),
            "a record read again that names another start says gone"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A daemon that ended and a new one that took its pid are 2 daemons. The record of the new one
    /// names a live process, so its own check passes: only the start that discovery read tells them apart.
    #[test]
    fn a_new_daemon_with_the_pid_of_the_discovered_one_is_not_signalled() {
        let base = scratch("reused-pid");
        let state = base.join("x.json");
        let own = std::process::id();
        let lock = write_record(&base, "one-two", (own, own_start()), &state);
        let mut discovered = target("m", "one-two", own);
        discovered.lock = lock;
        discovered.start = own_start() + 1;

        assert!(
            !runs_now(&discovered),
            "the record names a live process with the pid of the target and another start"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_signal_needs_the_same_process_a_pid_above_1_and_a_live_daemon() {
        assert!(
            may_signal((10, 5), (10, 5), DaemonState::Live),
            "the daemon that runs"
        );
        assert!(
            !may_signal((10, 5), (11, 5), DaemonState::Live),
            "a record of another pid"
        );
        assert!(
            !may_signal((10, 5), (10, 6), DaemonState::Live),
            "a record of a new process that took the pid"
        );
        assert!(
            !may_signal((10, 5), (10, 5), DaemonState::Gone),
            "a daemon that is gone"
        );
        assert!(
            !may_signal((10, 5), (10, 5), DaemonState::Unknown),
            "a daemon that may run is not signalled"
        );
        assert!(
            !may_signal((1, 5), (1, 5), DaemonState::Live),
            "init is never signalled"
        );
        assert!(
            !may_signal((0, 5), (0, 5), DaemonState::Live),
            "pid 0 would signal a process group"
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
    fn a_session_pid_of_init_or_below_names_no_session() {
        for pid in [0, 1] {
            let error = session_anchor(Some(pid), Some(500))
                .expect_err("pid 0 and pid 1 are ancestors of every process")
                .to_string();
            assert!(
                error.contains("init or invalid") && error.contains(&pid.to_string()),
                "the error says which pid and why: {error}"
            );
        }
    }

    #[test]
    fn a_default_anchor_that_is_init_or_unknown_names_no_session() {
        for parent in [None, Some(0), Some(1)] {
            assert!(
                session_anchor(None, parent).is_err(),
                "a parent of {parent:?} would select the daemons of every session"
            );
        }
    }

    #[test]
    fn a_real_session_pid_is_the_anchor_and_the_flag_wins_over_the_parent() {
        assert_eq!(
            session_anchor(None, Some(500)).expect("a normal parent names the session"),
            500,
            "without the flag the parent names the session"
        );
        assert_eq!(
            session_anchor(Some(700), Some(500)).expect("a normal pid names the session"),
            700,
            "the flag names the session before the parent does"
        );
    }
}
