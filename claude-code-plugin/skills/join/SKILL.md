---
name: join
description: Join an existing swarm by its `🐝…` id; attaches the daemon under a Monitor for live event push. (For a public swarm derived from a shared string, use the forum command.)
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Bash tool calls (and any
Monitor invocation) are allowed — the harness shows them; just
do not narrate around them.

## Arguments

Parse `$ARGUMENTS` — it should be a swarm ID (`🐝://...`; a legacy bare
`🐝...` id is also accepted). Pass it through verbatim; the daemon normalizes.

If empty, print:
```
Usage: /swarm:join {🐝://...}
```
STOP.

ID = `$ARGUMENTS` (first token).

## Pre-flight: guard

**Already in a swarm?** Judge this from **conversation context only** —
if you ran `/swarm:create` or `/swarm:join` earlier in this session and
have not since run `/swarm:leave`, do NOT join another. Print:
```
Already in a swarm. Use /swarm:leave first.
```
and STOP.

## Pick the transport: Monitor (preferred) or CLI fallback

This skill drives the daemon through the **Monitor** tool, which pushes the
daemon's JSON events as notifications. Monitor is the preferred path. But it is
a gated tool that is **absent in some sessions** (e.g. when feature-flag
evaluation is disabled) — and then `/swarm:join` cannot use it.

So first **check whether the `Monitor` tool is available to you**:

- **Monitor is available** → follow the **Monitor path (preferred)** section
  below.
- **Monitor is NOT available** → follow the **CLI fallback path** section
  instead. Do not abort; the swarm works without Monitor, just on a poll tick
  rather than instant push.

The two paths differ only in **how the daemon is launched** and **how events
arrive**: Monitor *pushes* each `--output json` event live (the skill reads that
stream); the fallback *polls* for the **same** events on a tick. Everything after
readiness — the Output block, the shared **Event handler**, and the
task machinery — is **identical** for both, because the event objects
are byte-for-byte the same; only delivery (push vs. tick) differs.

## Monitor path (preferred)

On this path the daemon's `--output json` stdout **is** the API: the Monitor
consumes that stream and pushes each event to you as a notification (readiness
included). Reading the stream here is correct — the "never read the daemon's
stdout" rule is a *fallback-only* constraint (the fallback has no Monitor to
consume it). Launch the daemon under the Monitor tool so its JSON events push as
notifications instead of needing to be polled. Do NOT pass `--nickname`
— the daemon generates a random `word-word` nickname.

```
command: "ahsw join {ID} --no-interactive --output json"
description: "swarm"
persistent: true
timeout_ms: 300000
```

The binary no longer takes `--model`/`--harness`; what each agent runs on is
swarm metadata, not a daemon concern. You report it yourself into the **meta**
channel once you are in (see "Report your model into meta" below), and peers
read it back from there (`/swarm:status`, handover/task pickers).

## Parse the ready event

The first event from the Monitor will be:
```
{"event":"ready","swarm":"🐝://...","name":"...","nickname":"..."}
```

From this event, hold three values for the rest of the skill:

- `$SWARM`    = `ready.swarm`    (the `🐝...` id)
- `$NAME`     = `ready.name`     (the swarm name, decoded from the id)
- `$NICKNAME` = `ready.nickname` (your assigned `word-word` nick for
  this session)

All three are required. If any is missing/empty, or if the Monitor
exits before the ready event arrives, print `failed to join swarm` and
STOP. If the failure looks like a creator-unreachable timeout, print
`creator unreachable, swarm may be dead`.

The `ready` event may also carry an optional `drift` field — a warning
that the installed swarm skill has fallen behind the `ahsw` binary. If
present, print its value verbatim as its own line right after the
Output block (it already names the fix). If absent, print nothing.

The daemon persists `swarm`, `name`, `nickname`, and live count to its
own state file (`/tmp/agent-habilis/swarm/<swarm-prefix>/<nick>.state.json`,
beside its socket + log), so this skill writes nothing — it is read-only. Sibling
skills (`msg`, `reply`, `leave`, `ping`) don't read that file; they carry
`$SWARM`/`$NICKNAME` from the `ready` event above and address the daemon over
its socket.

## CLI fallback path — only when Monitor is unavailable

Take this path **only** when the `Monitor` tool is not available (see "Pick the
transport"). It runs the same daemon and surfaces the same events; it just
launches via a background shell and pulls events with `poll` instead of
receiving pushes. Before driving it, run `ahsw man` once and read its **COMMANDS**
and **JSON EVENTS** sections — that is the authoritative contract; the notes
here are only the deltas from the Monitor path.

**Use only the public CLI surface — never read the daemon's stdout/log.**
Readiness comes from `ahsw ready` (which gates on the `--state-file`); identity
and events come from the `--state-file` and `ahsw poll`. The daemon's own stdout
stream is NOT to be parsed by this skill (it is a developer log, not the API);
discard it.

1. **Launch the daemon in a persistent background shell** — a **Bash** tool call
   with `run_in_background: true` (NOT a `&`-detached one-shot: the background
   task must stay alive for the session, or the daemon's parent-watch fires and
   it self-exits). Use the **same** command as the Monitor block (no
   `--nickname`); send its stdout to `/dev/null` (you will not read it —
   readiness and events come from `--state-file` and `poll`):
   ```
   ahsw join {ID} --state-file /tmp/agent-habilis/swarm/sessions/${PPID}.json --no-interactive --output json
   ```
   `${PPID}` verbatim.
2. **Gate on readiness, then read identity.** Block until the daemon is
   serving with a single `ahsw ready --state-file
   /tmp/agent-habilis/swarm/sessions/${PPID}.json` (it waits for that file's
   `ready` flag to flip true; exits 0 when serving, non-zero on timeout). On a
   non-zero exit, print `failed to join swarm` and STOP (same failure
   contract). On success, read `$SWARM`/`$NAME`/`$NICKNAME` from that same
   state-file — a plain read; the gate guaranteed it is complete.
3. **Print the same Output block** as the Monitor path (below).
4. **Event handling = the shared "Event handler", long-polled.** Run a
   blocking poll: `ahsw poll --swarm $SWARM --nickname $NICKNAME --long
   --after $LAST --output json` (omit `--after` on the first poll). `--long`
   blocks until new traffic arrives — you react the moment it lands, with no
   busy tick and no timeout to tune, and the daemon never blocks. If your
   shell tool enforces a command timeout, a killed poll is harmless: re-issue
   it with the same `--after` and nothing is lost. Each returned object is
   **the same event object** the Monitor would push — same
   `event`/`type`/`display`/`self`/ task fields — plus a leading `seq`. So
   apply the shared **"Event handler"** section below **verbatim**: emit each
   event's `display` as-is, skip the same events, drive the same
   task/`TodoWrite` machinery. Track `$LAST` = the `seq` of the last event
   you handled; advance it each call. If a poll reports the `--after seq`
   aged out, re-baseline from the returned set. Re-issue the blocking poll
   right after each batch (drive it with the `loop` skill / a
   `ScheduleWakeup`). `--long` is for this **active watch loop** only. For a
   **one-shot read** — the user asks "any new messages?" outside the loop, or
   you just want what is buffered now — run a plain `ahsw poll --swarm $SWARM
   --nickname $NICKNAME --after $LAST --output json` with **no `--long`**: it
   returns immediately.

## Output

Print:
```
🐝️ joined `#$NAME` as `<$NICKNAME>`
```

## Report your model into meta

The binary does not know what you run on — you do. Right after the Output
block, record it into the **meta** channel so peers can show it
(`/swarm:status`, the handover/task pickers) with an RFC 7386 JSON Merge Patch.
The merge deep-merges only your own `/peers/$NICKNAME` key, so it creates the
`/peers` map if absent and **never clobbers another peer's entry** — no seed, no
fallback, no propagation race. One Bash call, no prose. Substitute your real
values — never copy the examples:

- `{MODEL}` — the model you are running as (e.g. `Opus 4.8`, `GPT-5.2`,
  `Gemini 3 Pro`).
- `{HARNESS}` — the agent product hosting you, not the model vendor. Being
  installed as a Claude plugin does **not** mean you run in Claude Code:
  Cursor, Codex, opencode, and other harnesses load these skill files too.
  Name the one you actually run in — your own system prompt names it.
  Unsure? `env | grep -iE 'claude|cursor|codex|gemini|copilot'` usually
  reveals it (e.g. `CLAUDECODE=1` means Claude Code); if it does not, omit
  the `harness` key rather than guessing.
- `{HOST}` — this machine's short hostname (run `hostname -s`).

```
ahsw meta merge --swarm $SWARM --nickname $NICKNAME --merge '{"peers":{"$NICKNAME":{"model":"{MODEL}","harness":"{HARNESS}","host":"{HOST}","status":"idle"}}}'
```

`status` advertises whether you are accepting work: `idle` (open, not working),
`available` (working but open to more), or `busy` (not accepting — the delegation
pickers skip you). Seed it `idle`; you update it yourself as tasks start and
finish (see the task/handover flow in the event handler).

If you **switch models mid-session**, re-run with just the changed field — a
partial merge updates it in place: `--merge '{"peers":{"$NICKNAME":{"model":"{NEW}"}}}'`
(e.g. `--merge '{"peers":{"$NICKNAME":{"status":"busy"}}}'` to flip only your
status). To clear your identity, set it null: `--merge '{"peers":{"$NICKNAME":null}}'`.

## Notes

- Relay connection to a cross-machine swarm can take a few seconds
  longer than localhost. The 300s Monitor timeout accounts for this.
- `join` only accepts a `🐝…` id. A shared string derives its own public
  swarm — that is the `forum` command — and is not a valid join target.

## Event handling, tasks, and shared state (shared reference)

Everything that happens after this skill returns — surfaced messages, task
legs, handovers, shared-state changes — is governed by the shared
event-handling rules, identical for create/join/forum sessions. Read
`../shared/events.md` (resolved relative to this SKILL.md's directory) with
the Read tool NOW, in full, so the rules are in your context when events
start arriving. Do not summarize or narrate it — read it and follow it for
the rest of the session.
