---
name: create
description: Create a new swarm and attach the local daemon under a Monitor. Use when the user wants to start a new swarm session with a fresh `ahsw…` join id.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Bash tool calls (and any
Monitor invocation) are allowed — the harness shows them; just
do not narrate around them.

## Pre-flight: guard

**Already in a swarm?** Judge this from **conversation context only** —
if you ran `/swarm:create` or `/swarm:join` earlier in this session and
have not since run `/swarm:leave`, do NOT create another. Print:
```
Already in a swarm. Use /swarm:leave first if you want to create a new one.
```
and STOP.

## Resolve the swarm name

`ahsw create` takes an **optional** `--name {NAME}`. When given, the name is
1-32 UTF-8 characters (any script/emoji), excluding control characters,
whitespace, and any of `/ \ < > #`. It is bound cryptographically into the
swarm identity — joiners decode it from the swarm ID, and a forged name will
not find peers. When omitted, the daemon mints a random `word-word` name (the
same style as a nickname).

If the user passed a name as an argument to the skill, use it — the CLI is the
final validator, so pass it through and let `ahsw` reject a bad one. Otherwise
do **not** prompt: omit `--name` entirely and let the daemon mint a random
name. Never pass an empty `--name ""` (the CLI rejects it). The actual name
comes back in the `ready` event either way.

## Pick the transport: Monitor (preferred) or CLI fallback

This skill drives the daemon through the **Monitor** tool, which pushes the
daemon's JSON events as notifications. Monitor is the preferred path. But it is
a gated tool that is **absent in some sessions** (e.g. when feature-flag
evaluation is disabled) — and then `/swarm:create` cannot use it.

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
notifications instead of needing to be polled:

```
command: "ahsw create [--name {NAME}] --model {MODEL} --harness 'Claude Code' --state-file /tmp/agent-habilis/swarm/sessions/${PPID}.json --no-interactive --output json"
description: "swarm"
persistent: true
timeout_ms: 300000
```

Include `--name {NAME}` only when the user supplied a name; omit the flag
entirely otherwise (do not pass an empty value).

Set `--model {MODEL}` to your own model name (e.g. `'Opus 4.8'`) and keep
`--harness 'Claude Code'` as the constant for this plugin. These are
self-reported so peers can show what each agent runs on (`/swarm:status`,
handover/task pickers). Quote any value containing a space.

The Monitor runs the command in the same shell environment as Bash, so
`${PPID}` expands to the parent Claude Code process — the same per-agent
key the sibling skills (`msg`, `leave`, …) use to find this file. Type
`${PPID}` verbatim into the command; do not substitute it yourself.

Add `--public` if the user requests cross-network connectivity (e.g.
connecting from different machines or networks). Add `--relay {URL}`
together with `--public` to pin a custom relay.

Add `--advertise[={DIRECTORY}]` when the user wants the swarm listed in a
directory so others can find it with `ahsw discover` (no id to share) — it
requires the public network, so add `--public` too. Bare `--advertise` ⇒ the
well-known `global` directory; `--advertise {DIRECTORY}` ⇒ a named one. When
you add it, hold the directory name as `$DIRECTORY` (the value you passed, or
`global` when bare) for the Output below; otherwise leave `$DIRECTORY` unset.

## Parse the ready event

The first event from the Monitor will be:
```
{"event":"ready","swarm":"ahsw...","name":"...","nickname":"..."}
```

From this event, hold three values for the rest of the skill:

- `$SWARM`    = `ready.swarm`    (the `ahsw...` id)
- `$NAME`     = `ready.name`     (the swarm name)
- `$NICKNAME` = `ready.nickname` (your assigned `word-word` nick)

All three are required. If any is missing/empty, or if the Monitor
exits before the ready event arrives, print `failed to create swarm`
and STOP.

The `ready` event may also carry an optional `drift` field — a warning
that the installed swarm skill has fallen behind the `ahsw` binary. If
present, print its value verbatim as its own line right after the
Output block (it already names the fix). If absent, print nothing.

The self-presence `joined` event arriving in the same Monitor batch is
redundant with the output below — skip it.

The daemon persists `swarm`, `name`, and `nickname` to the
`--state-file` path, so this skill writes nothing — it is read-only.
Sibling skills (`msg`, `reply`, `leave`, `ping`) read those
keys from there.

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
   it self-exits). Use the **same** command as the Monitor block; send its
   stdout to `/dev/null` (you will not read it — readiness and events come from
   `--state-file` and `poll`):
   ```
   ahsw create [--name {NAME}] --model {MODEL} --harness 'Claude Code' --state-file /tmp/agent-habilis/swarm/sessions/${PPID}.json --no-interactive --output json
   ```
   Same flag rules as above (`--name`/`--public`/`--advertise`/`--relay`,
   `${PPID}` verbatim).
2. **Gate on readiness, then read identity.** Block until the daemon is
   serving with a single `ahsw ready --state-file
   /tmp/agent-habilis/swarm/sessions/${PPID}.json` (it waits for that file's
   `ready` flag to flip true; exits 0 when serving, non-zero on timeout). On a
   non-zero exit, print `failed to create swarm` and STOP (same failure
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

Print (include the `advertising` line **only** when you added `--advertise`;
`$DIRECTORY` is the directory you advertised into, `global` if bare):
```
🐝️ created `#$NAME` and joined as `<$NICKNAME>`
advertising on `#$DIRECTORY`
others can join with: `/swarm:join $SWARM`
```
Omit the `advertising` line entirely when not advertising.

## Offer to copy the join command

After the Output block, offer to put the join command on the clipboard so the
user can share it without hand-selecting it. Use the **ask widget**
(`AskUserQuestion`) — it is a tool, not prose, so it is allowed under Quiet
mode:

- question: "Copy the join command to your clipboard?"
- header: "Clipboard"
- options (single-select): **"Copy join command"** and **"Not now"**.

On **Copy join command**, run this portable clipboard command, substituting
the real `$SWARM` id (macOS `pbcopy`, with Wayland/X11 Linux fallbacks):

```bash
printf %s '/swarm:join $SWARM' | (pbcopy || wl-copy || xclip -selection clipboard || xsel -ib) 2>/dev/null
```

Then print exactly one line:
```
🐝 join command copied to clipboard
```

On **Not now**, do nothing.

The string copied **must** be byte-identical to the Output's join line
(`/swarm:join $SWARM`) so the two never drift. The ask widget plus that single
`🐝` confirmation line are the **only** additions allowed beyond the Output
block — no other narration (Quiet mode still holds otherwise).

## Notes

- The Monitor holds the daemon for the session lifetime. Use
  `/swarm:leave` to TaskStop it cleanly.
- Swarm IDs encode network mode AND the swarm name, so the join hint is
  always: `/swarm:join {ahsw...}`

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
derived document after the change), and `self`. Emit its `display` verbatim
like any other line.

- **`self:false` (a peer changed state) — react.** The `document` is already
  in your turn. Read it and act **per your current task**, but only if it is
  your turn (check a turn marker in the document — after you change state your
  own patch flips it to the peer). Read state any time with `ahsw state get
  --swarm $SWARM --nickname $NICKNAME`; change it with `ahsw state patch --swarm
  $SWARM --nickname $NICKNAME --patch '<RFC 6902 ops>'` (frozen subset:
  add/replace/remove on object paths + add `/arr/-`). Then stop — your patch
  wakes the peer. Don't encode app logic here; you decide what to do.
- **`self:true` (your own change)** — show its `display` (the confirmation),
  don't react. On join, let state settle a moment, then `ahsw state get` before
  acting.

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
   entry widget (`AskUserQuestion`): "Incoming task from `<author>`:
   *[one-line task]*. Run it?", header `swarm:task`, options **"Accept"** /
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
