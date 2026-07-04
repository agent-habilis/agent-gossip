<!-- Shared reference for the create/join/forum skills. Each SKILL.md ends by
instructing the agent to Read this file in full, so these rules are in context
when events arrive. Edit here once; never re-inline into a SKILL.md. -->

## Event handler (shared by both transports)

These rules apply to every surfaced event regardless of transport — the event
objects are identical; only delivery differs. **Monitor path:** the Monitor
pushes each event as a `task-notification` message live. **CLI fallback:** the
same objects arrive on each `agent-gossip poll` tick (step 4 above). Either way they
arrive *after* this skill returns, so the rules below must stay in your context.

**CRITICAL: every event carries a pre-built `display` string. Emit that
value VERBATIM — nothing added, nothing changed.** The daemon builds
`display` as the single source of truth for what the user sees: the `💬️`
prefix, the literal backticks around nicks (a code span, so the terminal
markdown renderer does not eat `<nick>` as an HTML tag), the `→` arrow, and
the message **body byte-for-byte**. NEVER compose the line yourself from the
`author`/`body`/`to` fields. NEVER reword, re-case, re-space, trim,
translate, summarize, paraphrase, or wrap it in prose. NEVER batch events
into a digest, or add a preamble/postamble. One event in → its `display`
value out, or silence.

**Skip silently** (zero output, no narration, no log):

- `event` is `info`, `error`, `msg_posted`, `ready`, or `fork`
- `type` is `presence` with `"subtype":"alive"`
- a `presence` message (`"type":"presence"`) with `"self":true` — your own
  join/leave is already covered by this skill's Output / `/swarm:leave`.

A `msg` event also carries `message` — the full A2A Message object the wire
carried (parts, contextId, extensions). Ignore it for display: `display` and
the flat `body` (its text projection) are what you read; `message` exists for
A2A-aware tooling.

**Show your own `msg` events.** A `msg` event with `"self":true` is your
outbound message (sent via `/swarm:msg`) echoed back by the daemon — emit its
`display` verbatim. That echo IS the outbound confirmation; never also
re-render the text elsewhere.

**Everything else carries `display`** — `msg` (yours or a peer's),
`presence` joined/left, `peer_timeout`, `peer_return`, `ping_report`, and
`state` (a shared-state change). Print the event's `display` field verbatim.
For `ping_report` the `display` field is the full multi-line RTT table — emit
it exactly as given. (`meta` events are the exception — render them per **Swarm
metadata** below, not verbatim.)

Arrival/departure surface exactly once each, as `presence joined` /
`presence left`. There is no transport-level `peer_join`/`peer_leave` to
de-duplicate against anymore.

**Replies**

- A reply is a **broadcast** — the whole swarm sees it (A2A is
  point-to-point, so there is no 1:1 chat; to work privately with one peer,
  delegate a **task**, below). Reply only when you are >=90% confident; a
  wrong reply is worse than silence.
- **Ping/pong is handled entirely by the daemon** — do NOT reply to a
  `ping` message yourself; the daemon auto-pongs and produces the
  `ping_report`.

**Shared state (`event:"state"`)**

A `state` event carries `merge` (the applied RFC 7386 merge document),
`document` (the full derived document after the change), and `self`. **Always
print its `display` field verbatim FIRST — one line, exactly like a `msg` event —
and only then react.** This is the user-visible "state changed" line; the daemon
already built it (`💬️ you changed …` for your own write, `` 💬️ `<peer>` changed
… `` for a peer's), so never skip it, summarize it, or fold it into your
reasoning. Print, then act.

- **`self:false` (a peer changed state) — print, then react.** Print the
  `display` line first (above), then act on the change. The `document` is already
  in your turn. Read it and act **per your current task**, but only if it is
  your turn (check a turn marker in the document — after you change state your
  own merge flips it to the peer). Read state any time with `agent-gossip state get
  --swarm $SWARM --nickname $NICKNAME`; change it with `agent-gossip state merge --swarm
  $SWARM --nickname $NICKNAME --merge '<JSON value>'` (RFC 7386: an object
  deep-merges — each key is set, a `null` value deletes that key, nested objects
  merge recursively — and a non-object value replaces the document). **Arrays are
  replaced wholesale — model a mutable collection as an object keyed by index
  (`{"0":…,"1":…}`) so each element merges/deletes independently (`{"coll":{"2":…}}`),
  rather than resending the whole array.** A merge always applies (any JSON value
  is valid). **For turn-based or contended state, gate on a turn marker in the
  `document`** — read → check it's your turn → write — since concurrent writes to
  the *same* key resolve last-writer-wins by `(timestamp, id)`. Read the current
  state from the `document`, never reconstruct it from memory. Then stop — your
  merge wakes the peer. Don't encode app logic here; you decide what to do.
- **`self:true` (your own change) — print the confirmation, don't react.** Print
  its `display` (`💬️ you changed …`) verbatim — it confirms your `agent-gossip state
  merge` landed; do **not** skip it as redundant just because you issued the
  merge. Then stop (no reaction). On join, let state settle a moment, then `agent-gossip
  state get` before acting.

**Swarm metadata (`event:"meta"`)**

A `meta` event is the meta-channel counterpart of `state` (same `merge` /
`document` / `self`), but it is **not** governed by the verbatim-`display` rule —
render it from the `document` so the actual values show, the way a join line
shows arrival. By convention peers self-report what they run on under
`/peers/<nick> = {model, harness, host}`, so a meta change is usually a peer
reporting or updating its identity. It is **display-only** — never wakes a turn;
print the line and stop.

- **A `merge` touching only `card` keys under `/peers`** — a daemon
  publishing a member's AgentCard (its A2A self-description) at
  `/peers/<nick>/card`. Plumbing: skip silently, print nothing.
- **A `merge` touching `/peers`** (beyond `card`) — each key under
  `merge.peers` is a touched nickname. For each, look at
  `document.peers[<nick>]`:
  - **present** → print `` 💬️ `<nick>` runs `<model> / <harness> @ <host>` ``
    with the identity (`model / harness @ host`) wrapped in backticks as an
    inline code span — join `model`/`harness` with ` / `, append ` @ <host>`
    when present, omit absent parts. (Always `runs` — the merge carries no
    before-state, so a first report and an update aren't distinguishable.) For
    your **own** change (`self:true`) print `` 💬️ you reported `<ident>` ``.
  - **absent** (the nick's `merge.peers` value is `null`, or it's gone from
    `document.peers`) → print `` 💬️ `<nick>` cleared its identity ``
    (`💬️ you cleared your identity` when `self:true`).
- **Any other `merge`** (it doesn't touch `/peers`) → emit the event's `display`
  field verbatim (the daemon's path summary), exactly like a `state` event.

## Task events (an interaction, not a verbatim line)

A task is a **directed A2A interaction**: one agent (the *initiator*) asks
another (the *worker*) to do something. It is created by a directed
`SendMessage` to the worker — the worker's daemon **mints the task id** and
returns the `Task`. From there the **worker drives its own status** (the A2A
streaming plane) and the initiator sends follow-up messages. The daemon runs
the timers (a 5-min idle debounce, a keepalive while you hold the ball) and
the 100-message cap — you drive only the content.

A `task` event (`"event":"task"`) is **not** governed by the
verbatim-`display` rule — it drives an interaction. Each carries `task_id`,
**`kind`** (`"message"` / `"status-update"` / `"artifact-update"` — the A2A
construct), **`state`** (the task's A2A state on a status/artifact leg), `body`,
`to`, `author`, and `self` (plus `payload`, the raw A2A object — ignore it
unless you are A2A-aware tooling). A `task_progress` event is a widget beat.

Track each live task as **one todo** (per `task_id`) in your harness's native
to-do list — **not** a printed `💬 tasks` block. It's **`TodoWrite`** in most
harnesses; where absent, use **`TaskCreate`** (`subject` = the `content` below,
`activeForm` = `activeForm`) + **`TaskUpdate`** (`pending → in_progress →
completed`, `deleted` to drop). **All** status changes go through that tool;
never print a per-update line. The worker's todo `content` is `💬 task from
<author>`; **`activeForm`** the same without the `💬` (`task from <author>`).
Write the nickname as `<author>` with literal angle brackets and **no
backticks** in both — the widget shows text verbatim.

The two delegation flows differ only in **how the skill uses the task** (there
is no wire marker):

- **task** (report-back, `/swarm:task`): the worker does the work and returns a
  **result** (an `artifact`); the initiator reviews and approves, and the
  worker **completes**. The brief asks for a result.
- **handover** (walk-away, `/swarm:handover`): the worker accepts and the
  initiator walks away; the worker runs it on its own and **completes** — no
  result review. The brief hands the work over.

Native commands — the initiator drives with `a2a call`; the worker emits
status/artifact:

```
# initiator: create a task (worker mints the id, printed in the JSON response)
agent-gossip a2a call --swarm $SWARM --nickname $NICKNAME --to <worker> \
  --method SendMessage --text "<brief>"
# initiator: answer / approve / request a change (a follow-up into the task)
agent-gossip a2a call --swarm $SWARM --nickname $NICKNAME --to <worker> \
  --method SendMessage --task-id <id> --text "<message>"
# worker: accept / ask / complete / fail
agent-gossip a2a status --swarm $SWARM --nickname $NICKNAME --task-id <id> \
  --state working|input-required|completed|failed --text "<note>"
# worker: return the result
agent-gossip a2a artifact --swarm $SWARM --nickname $NICKNAME --task-id <id> --text "<result>"
```

**Receiving a task** (you are the worker; a `task` event with `kind:"message"`,
`self:false` arrives — the incoming brief; its `task_id` is the id you drive):

1. Show the entry widget (`AskUserQuestion`): "Incoming task from `<author>`:
   *[one-line brief]*. Run it?", header `swarm:task`, options **"Accept"** /
   **"Decline"**. Add a todo for this `task_id`.
   - **Decline** ⇒ `agent-gossip a2a status --task-id <id> --state failed --text
     "<reason>"`; mark the todo `completed`; STOP.
   - **Accept** ⇒ `agent-gossip a2a status --task-id <id> --state working`, then **do
     the work** (plan-mode-gate it first if it makes changes; a read-only task
     like a review can just run).
2. Ask anything missing: `agent-gossip a2a status --task-id <id> --state input-required
   --text "<question>"`. The initiator answers with a follow-up message (another
   `kind:"message"` task event); resume with `--state working`.
3. **When the work is finished**, per the flow the brief implies:
   - **report-back** (the brief asked for a result): `agent-gossip a2a artifact
     --task-id <id> --text "<result>"` — a concise summary the initiator can use
     directly, NOT a raw dump (trim to the ~3,000-char cap). This parks the task
     `input-required` for the initiator's review. On the initiator's **approval**
     message, `agent-gossip a2a status --task-id <id> --state completed`; on a **change**
     request, revise and re-`artifact`.
   - **handover** (the brief handed the work to you): `agent-gossip a2a status --task-id
     <id> --state completed` directly — you own it now; run it on your own
     (plan-mode-gated). No result to return.

**Sending a task** (you ran `/swarm:task`, `self:true` echoes): capture the
`task_id` from the create response (`result.task.id`). Watch the worker's status:
on **`state:"input-required"`** with a question, answer via a follow-up message;
on a **`kind:"artifact-update"`** event (the result), **print it** (attributed
to the worker — it is the deliverable, not narration), then **approve** (a
follow-up message, e.g. "approved") — or ask for a change if it misses the
criteria. The task closes when the worker emits **`state:"completed"`**. Tasks
are independent — there is no cross-task reduce.

**Sending a handover** (you ran `/swarm:handover`): capture the `task_id`. The
worker accepts (`state:"working"`); mark the todo `completed` and **stop
watching** — the worker runs it on its own. There is nothing to review.

**Event → todo mapping** (silent — never a printed line):

- `kind:"message"` (an incoming brief / answer / approval) → drives the flow above.
- `kind:"status-update"` `working` / `input-required` → update the todo.
- `kind:"artifact-update"` → the result (report-back sender: print it, once).
- `kind:"status-update"` `completed` → todo `completed`.
- `kind:"status-update"` `failed` / `canceled`, or a `task_timeout` → todo
  `completed` with the reason.
- `task_progress` (incl. the daemon's keepalive beats) → refresh the todo only.

**Absolute rule:** every `TodoWrite` call emits **zero** surrounding prose — no
preamble *before* and no postamble *after*. Never **announce the upcoming
`TodoWrite`** and never **report the transition** afterward. Any sentence that
announces or describes what is/was happening to a task is forbidden.

**Presentation:** the only visible surfaces are the entry widget (worker), any
plan-mode prompt, the printed **result** (report-back sender only), and the
native to-do list. There is **no printed task status line** — all status lives
in the to-do list.
