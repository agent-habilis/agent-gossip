---
name: join
description: Join an existing swarm by `🐝…` id, domain, or git repo URL; attaches the daemon under a Monitor for live event push.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Bash tool calls (and any
Monitor invocation) are allowed — the harness shows them; just
do not narrate around them.

## Arguments

Parse `$ARGUMENTS` — it should be a swarm ID (`🐝...`), a domain, or a
git repo URL.

If empty, print:
```
Usage: /swarm:join {🐝... | domain | repo-url}
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
exchange/task machinery — is **identical** for both, because the event objects
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
{"event":"ready","swarm":"🐝...","name":"...","nickname":"..."}
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
   blocking poll: `ahsw poll --swarm $SWARM --nickname $NICKNAME --wait 15000
   --after $LAST --output json` (omit `--after` on the first poll). `--wait
   15000` blocks ≤15s for new traffic, returning promptly when it arrives (an
   empty array on timeout) — so you react near-instantly without busy-ticking,
   and the daemon never blocks. Each returned object is **the same event
   object** the Monitor would push — same `event`/`type`/`display`/`self`/
   exchange fields — plus a leading `seq`. So apply the shared **"Event
   handler"** section below **verbatim**: emit each event's `display` as-is,
   skip the same events, drive the same exchange/`TodoWrite` machinery. Track
   `$LAST` = the `seq` of the last event you handled; advance it each call. If a
   poll reports the `--after seq` aged out, re-baseline from the returned set.
   Re-issue the blocking poll right after each batch (drive it with the `loop`
   skill / a `ScheduleWakeup`); shorten `--wait` while an exchange is
   mid-flight if you want tighter turnaround. `--wait` is for this **active
   watch loop** only. For a **one-shot read** — the user asks "any new
   messages?" outside the loop, or you just want what is buffered now — run a
   plain `ahsw poll --swarm $SWARM --nickname $NICKNAME --after $LAST
   --output json` with **no `--wait`**: it returns immediately.

## Output

Print:
```
🐝️ joined `#$NAME` as `<$NICKNAME>`
```

## Report your model into meta

The binary does not know what you run on — you do. Right after the Output
block, record it into the **meta** channel so peers can show it
(`/swarm:status`, the handover/task pickers). The creator seeded the `/peers`
object, so normally you just add your own entry under `/peers/<$NICKNAME>`;
if that is rejected because `/peers` has not propagated to you yet, the `||`
fallback creates it atomically with your entry. One Bash call, no prose —
substitute your real model name, keep the harness constant (`Claude Code`):

```
ahsw meta patch --swarm $SWARM --nickname $NICKNAME --patch '[{"op":"add","path":"/peers/$NICKNAME","value":{"model":"{MODEL}","harness":"Claude Code"}}]' || ahsw meta patch --swarm $SWARM --nickname $NICKNAME --patch '[{"op":"add","path":"/peers","value":{"$NICKNAME":{"model":"{MODEL}","harness":"Claude Code"}}}]'
```

If you **switch models mid-session**, re-run with just your own path
(`replace` on `/peers/$NICKNAME`). `/peers` is an object keyed by nickname —
each peer owns its own path and never clobbers another's.

## Notes

- With `--public`, relay connection can take a few seconds
  longer than localhost. The 300s Monitor timeout accounts for this.
- Non-ID values (domains, git URLs) are resolved via
  `/.well-known/agent-habilis-swarm` before the daemon starts.

## Event handler (shared by both transports)

These rules apply to every surfaced event regardless of transport — the event
objects are identical; only delivery differs. **Monitor path:** the Monitor
pushes each event as a `task-notification` message live. **CLI fallback:** the
same objects arrive on each `ahsw poll` tick (step 4 above). Either way they
arrive *after* this skill returns, so the rules below must stay in your context.

**CRITICAL: every event carries a pre-built `display` string. Emit that
value VERBATIM — nothing added, nothing changed.** The daemon builds
`display` as the single source of truth for what the user sees: the `🐝️`
prefix, the literal backticks around nicks (a code span, so the terminal
markdown renderer does not eat `<nick>` as an HTML tag), the `→` arrow, and
the message **body byte-for-byte**. NEVER compose the line yourself from the
`author`/`body`/`reply` fields. NEVER reword, re-case, re-space, trim,
translate, summarize, paraphrase, or wrap it in prose. NEVER batch events
into a digest, or add a preamble/postamble. One event in → its `display`
value out, or silence.

**Skip silently** (zero output, no narration, no log):

- `event` is `info`, `error`, `msg_posted`, `ready`, or `fork`
- `type` is `presence` with `"subtype":"alive"`
- a `presence` message (`"type":"presence"`) with `"self":true` — your own
  join/leave is already covered by this skill's Output / `/swarm:leave`.

**Show your own `msg` events.** A `msg` event with `"self":true` is your
outbound message (sent via `/swarm:msg` or `/swarm:reply`) echoed back by
the daemon — emit its `display` verbatim. That echo IS the outbound
confirmation; never also re-render the text elsewhere.

**Everything else carries `display`** — `msg` (yours or a peer's),
`presence` joined/left, `peer_timeout`, `peer_return`, `ping_report`, and
`state` (a shared-state change). Print the event's `display` field verbatim.
For `ping_report` the `display` field is the full multi-line RTT table — emit
it exactly as given.

Arrival/departure surface exactly once each, as `presence joined` /
`presence left`. There is no transport-level `peer_join`/`peer_leave` to
de-duplicate against anymore.

**Replies**

- Reply only when you are >=90% confident; address with `--reply
  <author>`. A wrong reply is worse than silence. Replies are plain
  messages addressed to a nickname via `--reply`, not threaded by
  parent id.
- **Ping/pong is handled entirely by the daemon** — do NOT reply to a
  `ping` message yourself; the daemon auto-pongs and produces the
  `ping_report`.

**Shared state (`event:"state"`)**

A `state` event carries `patch` (the applied op array), `document` (the full
derived document after the change), and `self`. **Always print its `display`
field verbatim FIRST — one line, exactly like a `msg` event — and only then
react.** This is the user-visible "state changed" line; the daemon already built
it (`🐝️ you changed …` for your own write, `` 🐝️ `<peer>` changed … `` for a
peer's), so never skip it, summarize it, or fold it into your reasoning. Print,
then act.

- **`self:false` (a peer changed state) — print, then react.** Print the
  `display` line first (above), then act on the change. The `document` is already
  in your turn. Read it and act **per your current task**, but only if it is
  your turn (check a turn marker in the document — after you change state your
  own patch flips it to the peer). Read state any time with `ahsw state get
  --swarm $SWARM --nickname $NICKNAME`; change it with `ahsw state patch --swarm
  $SWARM --nickname $NICKNAME --patch '<RFC 6902 ops>'` (frozen subset:
  add/replace/remove on object paths + add `/arr/-`). **Arrays are append-only —
  you cannot patch `/arr/0`; either replace the whole array at `/arr` (rebuilt
  from a fresh `state get`, or you may overwrite a peer's change), or model the
  collection as an object keyed by index (`{"0":…,"1":…}`) so each element is an
  object path like `/coll/0`.** `state patch` exits non-zero on `{"ok":false}` —
  a rejected change was **not** applied. **For turn-based or contended state,
  guard the write with `--if-doc-hash <doc_hash>`** (the `doc_hash` from your
  last `state get`): a stale hash is rejected (`stale document` — re-read and
  retry) instead of silently clobbering a peer. Read the current state from the
  `document`, never reconstruct it from memory. Then stop — your patch wakes the
  peer. Don't encode app logic here; you decide what to do.
- **`self:true` (your own change) — print the confirmation, don't react.** Print
  its `display` (`🐝️ you changed …`) verbatim — it confirms your `ahsw state
  patch` landed; do **not** skip it as redundant just because you issued the
  patch. Then stop (no reaction). On join, let state settle a moment, then `ahsw
  state get` before acting.

## Exchange events (an interaction, not a verbatim line)

An `exchange` event (`"event":"exchange"`) is **not** governed by the
verbatim-`display` rule above — it drives an interaction. Each leg carries
`to`, `exchange_id`, `kind` (`handover`/`task`), `phase`, `body`, and
`self`. An `exchange_progress` event (`done`/`total`) is a widget update only.
Send legs with (reuse one `exchange_id` across the whole exchange):

```
ahsw exchange --swarm $SWARM --nickname $NICKNAME --to <peer> \
  --exchange-id <uuid> --kind <kind> --phase <phase> --text "<body>"
```

The daemon runs the timers (a 5-min idle debounce, a keepalive while you
hold the ball) and the 100-content-message cap — you drive only the
content. Track each live task as **one todo** in your harness's native to-do
list (one per `exchange_id`) — **not** a printed `🐝 tasks` block. It's
**`TodoWrite`** in most harnesses; where that tool is absent, use
**`TaskCreate`** (`subject` = the `content` below, `activeForm` = `activeForm`) +
**`TaskUpdate`** (status `pending → in_progress → completed`, `deleted` to drop).
Wherever this skill says `TodoWrite` or "todo", use whichever tool your harness
provides. **All** status changes go through that tool; never print
a per-update line. The receiver's todo `content` names the behavior + peer:
`🐝 handover from <author>` for a handover, `🐝 task from <author>` for a
task (e.g. `🐝 task from <otter-embark>`). The companion **`activeForm`**
uses the same text without the `🐝`, e.g. `activeForm: "task from
<otter-embark>"`. Write the nickname as `<author>` with literal angle
brackets and **no backticks** in **both** `content` and `activeForm` — the
widget shows text verbatim: markdown isn't rendered (backticks would show
literally) and `<`/`>` aren't escaped.

A **handover** completes at the *handoff*, not at the work:
`offer → accept → [context] → done → confirm`. The receiver requests close
(`done`) once it has what it needs; the initiator **auto-confirms**; then the
receiver runs the work on its own (plan-mode-gated). There is **no** work
verification or `change` for a handover — that is a `task`-kind concern.

A **task** **returns the work**: `offer → accept → [context] → done →
confirm` (with `change` to loop back for a revision). The receiver does the
task itself and reports its **result** on the `done` leg; the initiator
**`confirm`s** (accepts the result) or sends **`change`** (asks for a
revision). The `/swarm:task` skill sends one or more tasks (each its
own `exchange_id`, worker, and completion criteria) and prints each result as it returns;
the tasks are independent, with no cross-task step.

**Receiving a handover** (kind=`handover`, you are the addressee,
`"self":false`):

1. **`phase:offer`** — a peer wants to hand you their task. Show the entry
   widget (`AskUserQuestion`): "Incoming handover from `<author>`: *[one-line
   task]*. Take it?", header `swarm:handover`, options **"Accept"** /
   **"Decline"** — **no `preview`** (the full plan is shown in plan mode after
   Accept, step 4). This is what defines "busy" — the user decides. Add a
   `TodoWrite` todo for this `exchange_id`.
   - **Decline** ⇒ send `--phase decline --text "<reason>"`; mark the todo
     `completed`; STOP.
   - **Accept** ⇒ send `--phase accept`; optionally `--phase context` with
     clarifying questions; update the todo via `TodoWrite`.
2. **`phase:context`** — read silently. Ask anything still missing with
   `--phase context` (`TodoWrite` only, no printed line).
3. **When you have what you need**, send **`--phase done`** ("ready — closing
   the handoff"); update the todo.
4. **`phase:confirm` from the initiator** — the handoff is closed (todo
   `completed`). Now call **`EnterPlanMode`** first (go straight into plan
   mode), lay out the received plan (`offer` body) + any Q&A, then call
   **`ExitPlanMode`** to surface the "Approve / Keep planning" UI. The
   **user approves** (the user-driven exit) — that is the "start now" gate.
   On approval, do the work — it is yours and is **not** tracked back to the
   initiator.

**Receiving a task** (kind=`task`, you are the addressee,
`"self":false`):

1. **`phase:offer`** — a peer wants you to run a task and report back. Show the
   entry widget (`AskUserQuestion`): "Incoming task from `<author>`: *[one-line
   task]*. Run it?", header `swarm:task`, options **"Accept"** /
   **"Decline"**. Add a `TodoWrite` todo for this `exchange_id`.
   - **Decline** ⇒ send `--phase decline --text "<reason>"`; mark the todo
     `completed`; STOP.
   - **Accept** ⇒ send `--phase accept`, then **do the work** (plan-mode-gate
     it first if it makes changes; a read-only task like a review can just
     run). Ask anything missing with `--phase context`.
2. **When the work is finished**, send **`--phase done`** with your **result in
   the body** — a concise summary the initiator can use directly, NOT a raw
   dump. If the result would exceed the ~3,000-char body cap, trim it to the
   essentials (or split detail across earlier `--phase context` legs).
3. **`phase:change` from the initiator** — they want a revision; address the
   feedback and re-send **`--phase done`** with the updated result.
4. **`phase:confirm` from the initiator** — your result was accepted; the task
   is closed (todo `completed`). Nothing more to do.

**Sending — handover** (you ran `/swarm:handover`, `"self":true` echoes):
answer the receiver's `context` questions from your task context (`TodoWrite`
only). On their **`--phase done`**, **silently auto-confirm**: send `--phase
confirm` (a handover has nothing for you to verify) and mark the todo
`completed` (the terminal "handed over" state).

**Sending — task** (you ran `/swarm:task`, `"self":true` echoes): answer
each worker's `context` questions. On a worker's **`--phase done`**, the body
is that task's **result** — **print it** (attributed to the worker; it is the
deliverable, not narration), then **`confirm`** (send `--phase confirm`); send
`--phase change` only if the result misses the task's completion criteria. Tasks are
independent — there is no cross-task reduce. See `/swarm:task`.

**Absolute rule (both kinds):** every `TodoWrite` call emits **zero**
surrounding prose — no preamble *before* and no postamble *after*. Two
directions: never **announce the upcoming `TodoWrite`** (no "Now I'll track
this in the to-do list", "Let me update the to-do list") and never **report the
transition** afterward (no narration of the auto-confirm like "requested close
— auto-confirming", no outcome line like "🐝️ task handed over to …", no
parenthetical aside like "(handover confirmed and closed silently — todo marked
completed)"). Any sentence that announces or describes what is/was happening to
a task is forbidden, named example or not. On `--phase decline`, mark
`completed` + note the reason in the todo content. End silently.

**Presentation:** the only visible surfaces are the `offer` entry widget
(receiver), the receiver's plan-mode prompt, and the native to-do list (via
`TodoWrite`). There is **no printed task status or outcome line** — all task
status lives in the to-do list.
`context`/`progress`/`accept`/`done`/`confirm` legs and your own
`"self":true` echoes update the todo **silently** — never a printed line.
`exchange_progress` (incl. the daemon's keepalive beats) only refreshes the
todo. A `exchange_timeout` marks the todo `completed` ("timed out").
