---
name: join
description: Join an existing swarm by `ahs…` id, domain, or git repo URL; attaches the daemon under a Monitor for live event push.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Bash tool calls (and any
Monitor invocation) are allowed — the harness shows them; just
do not narrate around them.

## Arguments

Parse `$ARGUMENTS` — it should be a swarm ID (`ahs...`), a domain, or a
git repo URL.

If empty, print:
```
Usage: /swarm:join {ahs... | domain | repo-url}
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

## Start the Monitor

Launch the daemon under the Monitor tool so its JSON events push as
notifications instead of needing to be polled. Do NOT pass `--nickname`
— the daemon generates a random `word-word` nickname.

```
command: "ah-s join {ID} --state-file /tmp/agent-habilis/swarm/sessions/${PPID}.json --no-interactive --output json"
description: "swarm"
persistent: true
timeout_ms: 300000
```

The Monitor runs the command in the same shell environment as Bash, so
`${PPID}` expands to the parent Claude Code process — the same per-agent
key the sibling skills (`msg`, `leave`, …) use to find this file. Type
`${PPID}` verbatim into the command; do not substitute it yourself.

## Parse the ready event

The first event from the Monitor will be:
```
{"event":"ready","swarm":"ahs...","name":"...","nickname":"..."}
```

From this event, hold three values for the rest of the skill:

- `$SWARM`    = `ready.swarm`    (the `ahs...` id)
- `$NAME`     = `ready.name`     (the swarm name, decoded from the id)
- `$NICKNAME` = `ready.nickname` (your assigned `word-word` nick for
  this session)

All three are required. If any is missing/empty, or if the Monitor
exits before the ready event arrives, print `failed to join swarm` and
STOP. If the failure looks like a creator-unreachable timeout, print
`creator unreachable, swarm may be dead`.

The `ready` event may also carry an optional `drift` field — a warning
that the installed swarm skill has fallen behind the `ah-s` binary. If
present, print its value verbatim as its own line right after the
Output block (it already names the fix). If absent, print nothing.

The daemon persists `swarm`, `name`, and `nickname` to the
`--state-file` path, so this skill writes nothing — it is read-only.
Sibling skills (`msg`, `reply`, `leave`, `ping`) read those
keys from there.

## Output

Print:
```
🐝️ joined `#$NAME` as `<$NICKNAME>`
```

## Notes

- With `--public`, relay connection can take a few seconds
  longer than localhost. The 300s Monitor timeout accounts for this.
- Non-ID values (domains, git URLs) are resolved via
  `/.well-known/agent-habilis-swarm` before the daemon starts.

## Monitor event handler (after join exits)

The Monitor pushes JSON events as `task-notification` messages for the rest
of the session. These arrive *after* this skill returns, so the rules below
must stay in your context.

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
`presence` joined/left, `peer_timeout`, `peer_return`, and `ping_report`.
Print the event's `display` field verbatim. For `ping_report` the `display`
field is the full multi-line RTT table — emit it exactly as given.

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

## Task events (an interaction, not a verbatim line)

A `task` event (`"event":"task"`) is **not** governed by the
verbatim-`display` rule above — it drives an interaction. Each leg carries
`to`, `task_id`, `kind` (`handover`/`execute`), `phase`, `body`, and
`self`. A `task_progress` event (`done`/`total`) is a widget update only.
Send legs with (reuse one `task_id` across the whole exchange):

```
ah-s task --swarm $SWARM --nickname $NICKNAME --to <peer> \
  --task-id <uuid> --kind <kind> --phase <phase> --text "<body>"
```

The daemon runs the timers (a 5-min idle debounce, a keepalive while you
hold the ball) and the 100-content-message cap — you drive only the
content. Track each live task as **one todo** in Claude Code's native to-do
list via the **`TodoWrite`** tool (one per `task_id`) — **not** a printed
`🐝 tasks` block. **All** status changes go through `TodoWrite`; never print
a per-update line. The receiver's todo `content` is **exactly**
`🐝 handover from <author>` (e.g. `🐝 handover from <otter-embark>`). The
todo `content` is **plain text shown verbatim** — write the nickname with
literal `<`/`>`, **no backticks** and **no HTML entities** (`&lt;`). The
companion **`activeForm`** (the spinner text) renders on a **different surface
that HTML-escapes `<`/`>`** (→ `&lt;…&gt;`), so it must use the **bare**
nickname with **no angle brackets**, e.g. `activeForm: "handover from
otter-embark"`. Never put `<`/`>` (or backticks, or entities) in `activeForm`
or any spinner/status text.

A **handover** completes at the *handoff*, not at the work:
`offer → accept → [context] → done → confirm`. The receiver requests close
(`done`) once it has what it needs; the initiator **auto-confirms**; then the
receiver runs the work on its own (plan-mode-gated). There is **no** work
verification or `change` for a handover — that is an `execute`-kind concern.

**Receiving (you are the addressee, `"self":false`):**

1. **`phase:offer`** — a peer wants to hand you their task. Show the entry
   widget (`AskUserQuestion`): "Incoming handover from `<author>`: *[one-line
   task]*. Take it?", header `swarm:handover`, options **"Accept"** /
   **"Decline"** — **no `preview`** (the full plan is shown in plan mode after
   Accept, step 4). This is what defines "busy" — the user decides. Add a
   `TodoWrite` todo for this `task_id`.
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

**Sending (you ran `/swarm:handover`, `"self":true` echoes):** answer the
receiver's `context` questions from your task context (`TodoWrite` only). On
their **`--phase done`**, **silently auto-confirm**: send `--phase confirm`
(a handover has nothing for you to verify) and mark the todo `completed` (the
terminal "handed over" state). **Absolute rule:** the auto-confirm and close
emit a `TodoWrite` call and **zero** prose — no narration of the auto-confirm
(no "requested close — auto-confirming"), no outcome line (no "🐝️ task handed
over to …"), and **no parenthetical aside** reporting the close (no "(handover
confirmed and closed silently — todo marked completed)"). Any sentence that
describes what just happened to the task is forbidden, named example or not.
On `--phase decline`, mark `completed` + note the reason in the todo content.
End silently.

**Presentation:** the only visible surfaces are the `offer` entry widget
(receiver), the receiver's plan-mode prompt, and the native to-do list (via
`TodoWrite`). There is **no printed task status or outcome line** — all task
status lives in the to-do list.
`context`/`progress`/`accept`/`done`/`confirm` legs and your own
`"self":true` echoes update the todo **silently** — never a printed line.
`task_progress` (incl. the daemon's keepalive beats) only refreshes the
todo. A `task_timeout` marks the todo `completed` ("timed out").
