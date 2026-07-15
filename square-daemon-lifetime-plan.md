# Daemon survives the harness: `--detach` + `--watch-pid` session ownership

## Status

Planned, not started. Investigation and design are complete and verified
against the code.

## Context

After a long MacBook sleep, Claude Code stopped the background task hosting
the square daemon, killing the daemon with it. The daemon's log proves the
engine handles sleep correctly (repeated "freeze/sleep" stalls each followed
by a hard re-bootstrap and re-mesh) and that it did not self-exit — the log
stops mid-operation: external kill. Requirement: the daemon reconnects across
sleep/wake (already true) and exits ONLY on `agent-square leave`, a signal,
or the real end of the agent session — never because harness task
supervision reaped a background task.

Fix: the daemon detaches itself from the harness task (in-binary re-exec
into a new session; macOS has no `setsid` binary) and ties its lifetime to
the *agent process* via an explicit watched pid, replacing the parent-ppid
orphan watch. Two verified constraints shape it:

- **`--detach` must require `--watch-pid`.** The existing orphan watch
  (`spawn_orphan_watch`, `crates/agent-habilis-mesh/src/daemon/event_loop.rs:757`)
  captures the launcher as parent; the launcher exits, ppid flips to 1, and
  the detached daemon would self-quit within ~1.5 s. The pid watch replaces
  it (and preserves the no-zombie guarantee: agent dies ⇒ daemon leaves).
- **The re-exec must null the child's stdio** or the foreground launcher
  call hangs on pipe EOF until the daemon exits.

## Changes

1. **Engine watch mode** — `crates/agent-habilis-mesh/src/daemon/event_loop.rs`:
   pure `lifetime_watch_mode(watch_pid, original_ppid) -> {Off, Ppid, Pid}`
   beside `orphan_watch_warranted`/`parent_lost`; `spawn_orphan_watch` →
   `spawn_lifetime_watch(quit_tx, watch_pid)`; `Pid` arm polls
   `process::is_alive` at `ppid_watch_interval_ms()` cadence (capture
   `comm_of(pid)` once and require it unchanged — pid-reuse hardening),
   dead ⇒ existing graceful quit path. Thread `watch_pid` through
   `spawn_quit_signal_tasks` (one call site, :177).
2. **Plumbing + state file** — `daemon/setup.rs` `SetupParams.watch_pid`,
   `daemon/config.rs` `EventLoopConfig.watch_pid`; `run()` chains
   `.with_session_pid(watch_pid)` onto `StateFile::new` (state file is a raw
   JSON map — additive `session_pid` key, compatible both directions);
   `state_file.rs`: `with_session_pid` builder + write-when-Some +
   `SessionEntry.session_pid` in `read_session_entry`. Non-CLI `SetupParams`
   sites (`api/session.rs:97`, `api/setup.rs:66,:137`) pass `None`.
3. **CLI flags** — `crates/agent-square/src/cli/args/shared.rs`
   (`SharedServerOpts`, visible, clap docs as help): `--detach`
   (`requires = "watch_pid"`) and `--watch-pid <PID>`.
4. **Detach re-exec** — new `cli/detach.rs`: `current_exe()` + argv minus the
   exact `--detach` token, `Stdio::null()` on all three, unix `pre_exec`
   `libc::setsid()` (single fork+setsid suffices — the daemon never opens a
   tty). Called at the top of the Create/Join/Topic dispatch arms (after
   clap, before `tuning::init`): print one `{"event":"detached","pid":N}`
   line, exit 0. State-file `pid` is written by the child at write time
   (`state_file.rs:124`) so it is automatically the real daemon pid.
5. **Ownership discovery (critical)** — `cli/session.rs`: `Target` carries
   `session_pid`; ownership predicate becomes
   `session_pid == Some(anchor) || ancestry_contains(pid, anchor)` at both
   `split_owned` sites (leave :159, session :212) — detached daemons match
   via the recorded pid (skills use the same `$PPID` for launch and leave),
   old/non-detached daemons keep matching via ancestry. Surface
   `session_pid` in `target_json`.
6. **Skills** — `skills/shared/daemon-session.md`: Tool call 1 becomes a
   quick FOREGROUND call:
   `<launch> --detach --watch-pid ${PPID} --state-file … > /dev/null 2>&1`;
   reframe the intro (one background task — the bell — and two foreground
   calls); one-line why (session-owned via watch-pid, so harness task
   supervision or a long sleep can never kill it). Bell (`bell_prefix`) and
   gate unchanged. `cli/agent.rs` test
   `long_running_square_commands_discard_stdout_and_stderr` (:393): reword
   its framing and add an assertion that the launch line carries
   `--detach --watch-pid`.
7. **Tests** — unit: `lifetime_watch_mode` truth table; state-file
   `session_pid` round-trip + old-shape tolerance; `session.rs` ownership
   helper; clap `--detach` requires `--watch-pid`. Subprocess
   (`gossip_network.rs` serial section, reuse Node/tmp_log/wait_until):
   (a) detached launcher exits fast, state file carries child pid + ppid 1 +
   `session_pid`, `session --session-pid` finds it, decoy anchor does not,
   `leave --session-pid` stops it;
   (b) killing the watched `sleep 300` makes the daemon exit gracefully
   (state file removed, observer sees `left`) with
   `--ppid-watch-interval-ms 200`.
   Keep `test_orphaned_daemon_self_terminates` untouched (pins legacy path).
8. **Docs** — `docs/manual.txt`: LIFECYCLE (watch-pid/detach) + state-file
   schema (`session_pid`).

## Risks

- Pid reuse on the watched pid: 1.5 s poll window + comm capture ⇒
  negligible; documented.
- Sandboxed harnesses could still reap by session; failure mode is the
  status quo, never worse.
- Child stderr is nulled: post-parse startup failures surface as a `ready`
  timeout + daemon log — same as today's discarded background output.

## Verification

- `cargo task lint`; `cargo task test` in the background.
- Manual: `agent-square topic smoke --detach --watch-pid $$ --state-file
  /tmp/as-test.json` returns immediately; `ps` shows the daemon with ppid 1
  in its own session; `session --session-pid $$` finds it; killing the
  watched shell removes daemon + state file; `leave` works.
- `cargo task install` + `agent-square plug`; live `/square-topic`; the
  acceptance test: daemon survives laptop sleep/wake and harness task
  cleanup, and `/square-leave` still stops it.
