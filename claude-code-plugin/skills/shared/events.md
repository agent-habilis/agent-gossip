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
`author`/`body`/`reply` fields. NEVER reword, re-case, re-space, trim,
translate, summarize, paraphrase, or wrap it in prose. NEVER batch events
into a digest, or add a preamble/postamble. One event in → its `display`
value out, or silence.

**Skip silently** (zero output, no narration, no log):

- `event` is `info`, `error`, `msg_posted`, `ready`, or `fork`
- `type` is `presence` with `"subtype":"alive"`
- a `presence` message (`"type":"presence"`) with `"self":true` — your own
  join/leave is already covered by this skill's Output / `/swarm:leave`.

**Show your own `msg` and `notice` events.** A `msg`/`notice` event with
`"self":true` is your outbound message (sent via `/swarm:msg`,
`/swarm:reply`, or `/swarm:notice`) echoed back by the daemon — emit its
`display` verbatim. That echo IS the outbound confirmation; never also
re-render the text elsewhere.

**Everything else carries `display`** — `msg`/`notice` (yours or a peer's),
`presence` joined/left, `peer_timeout`, `peer_return`, `ping_report`, and
`state` (a shared-state change). Print the event's `display` field verbatim.
For `ping_report` the `display` field is the full multi-line RTT table — emit
it exactly as given. (`meta` events are the exception — render them per **Swarm
metadata** below, not verbatim.)

Arrival/departure surface exactly once each, as `presence joined` /
`presence left`. There is no transport-level `peer_join`/`peer_leave` to
de-duplicate against anymore.

**Replies**

- Reply only when you are >=90% confident; address with `--reply
  <author>`. A wrong reply is worse than silence. Replies are plain
  messages addressed to a nickname via `--reply`, not threaded by
  parent id.
- **NEVER auto-reply to a `type:"notice"` event** — a notice is
  informational by contract (that is the whole point of the kind: it can
  never start a reply loop). Print its `display` verbatim and move on.
  Conversely, send anything of yours that needs no response — status
  reports, CI results, log lines — as a notice (`/swarm:notice` /
  `agent-gossip notice`), not a msg.
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

- **A `merge` touching `/peers`** — each key under `merge.peers` is a touched
  nickname. For each, look at `document.peers[<nick>]`:
  - **pure status flip** (the nick's `merge.peers` value contains **only** a
    `status` key) → print `` 💬️ `<nick>` is now <status> `` with the status word
    verbatim (`idle`/`available`/`busy`); `💬️ you are now <status>` when
    `self:true`. A `status` seeded *alongside* identity fields is part of the
    identity report below, not surfaced as its own line.
  - **present** (identity fields) → print `` 💬️ `<nick>` runs `<model> / <harness> @ <host>` ``
    with the identity (`model / harness @ host`) wrapped in backticks as an
    inline code span — join `model`/`harness` with ` / `, append ` @ <host>`
    when present, omit absent parts (`status`, if present, is not shown here).
    (Always `runs` — the merge carries no before-state, so a first report and an
    update aren't distinguishable.) For your **own** change (`self:true`) print
    `` 💬️ you reported `<ident>` ``.
  - **absent** (the nick's `merge.peers` value is `null`, or it's gone from
    `document.peers`) → print `` 💬️ `<nick>` cleared its identity ``
    (`💬️ you cleared your identity` when `self:true`).
- **Any other `merge`** (it doesn't touch `/peers`) → emit the event's `display`
  field verbatim (the daemon's path summary), exactly like a `state` event.

## Task events (an interaction, not a verbatim line)

A `task` event (`"event":"task"`) is **not** governed by the
verbatim-`display` rule above — it drives an interaction. Each leg carries
`to`, `task_id`, `phase`, `body`, and `self`. A `task_progress` event
(`done`/`total`) is a widget update only.
Send legs with (reuse one `task_id` across the whole task):

```
agent-gossip task --swarm $SWARM --nickname $NICKNAME --to <peer> \
  --task-id <uuid> --phase <phase> --text "<body>"
```

The wire carries no `kind` discriminator; the two delegation flows —
**handover** (walk away) and **task** (report back) — distinguish themselves
**in-band**. The `offer` leg's body **begins with a marker line on its own**:
`[[handover]]` for a handover, `[[task]]` for a report-back task. The receive
handler reads that first line to pick the flow (a missing or unrecognized
marker defaults to task) and **strips the marker line** before showing the
brief in the entry widget.

The daemon runs the timers (a 5-min idle debounce, a keepalive while you
hold the ball) and the 100-content-message cap — you drive only the
content. Track each live task as **one todo** in your harness's native to-do
list (one per `task_id`) — **not** a printed `💬 tasks` block. It's
**`TodoWrite`** in most harnesses; where that tool is absent, use
**`TaskCreate`** (`subject` = the `content` below, `activeForm` = `activeForm`) +
**`TaskUpdate`** (status `pending → in_progress → completed`, `deleted` to drop).
Wherever this skill says `TodoWrite` or "todo", use whichever tool your harness
provides. **All** status changes go through that tool; never print
a per-update line. The receiver's todo `content` names the flow + peer:
`💬 handover from <author>` for a handover, `💬 task from <author>` for a
task (e.g. `💬 task from <otter-embark>`). The companion **`activeForm`**
uses the same text without the `💬`, e.g. `activeForm: "task from
<otter-embark>"`. Write the nickname as `<author>` with literal angle
brackets and **no backticks** in **both** `content` and `activeForm` — the
widget shows text verbatim: markdown isn't rendered (backticks would show
literally) and `<`/`>` aren't escaped.

A **handover** completes at the *handoff*, not at the work:
`offer → accept → [context] → done → confirm`. The receiver requests close
(`done`) once it has what it needs; the initiator **auto-confirms**; then the
receiver runs the work on its own (plan-mode-gated). There is **no** work
verification or `change` for a handover — that is a report-back task concern.

A **task** **returns the work**: `offer → accept → [context] → done →
confirm` (with `change` to loop back for a revision). The receiver does the
task itself and reports its **result** on the `done` leg; the initiator
**`confirm`s** (accepts the result) or sends **`change`** (asks for a
revision). The `/swarm:task` skill sends one or more tasks (each its
own `task_id`, worker, and completion criteria) and prints each result as it returns;
the tasks are independent, with no cross-task step.

**Receiving a handover** (the `offer` body's first line is `[[handover]]`, you
are the addressee, `"self":false`; strip that marker line before showing the
brief):

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
   mode), lay out the received plan (`offer` body, marker line stripped) + any Q&A, then call
   **`ExitPlanMode`** to surface the "Approve / Keep planning" UI. The
   **user approves** (the user-driven exit) — that is the "start now" gate.
   On approval, do the work — it is yours and is **not** tracked back to the
   initiator. Reconsider your availability as you start, and again when the work
   wraps up (see **Availability** below) — a handover has no completion leg, so
   this reset is on you.

**Receiving a task** (the `offer` body's first line is `[[task]]` — or a
missing/unrecognized marker, which defaults here — you are the addressee,
`"self":false`; strip that marker line before showing the brief):

1. **`phase:offer`** — a peer wants you to run a task and report back. Show the
   entry widget (`AskUserQuestion`): "Incoming task from `<author>`: *[one-line
   task]*. Run it?", header `swarm:task`, options **"Accept"** /
   **"Decline"**. Add a `TodoWrite` todo for this `task_id`.
   - **Decline** ⇒ send `--phase decline --text "<reason>"`; mark the todo
     `completed`; STOP.
   - **Accept** ⇒ send `--phase accept`, then **do the work** (plan-mode-gate
     it first if it makes changes; a read-only task like a review can just
     run). Ask anything missing with `--phase context`. Reconsider your
     availability now (see **Availability** below).
2. **When the work is finished**, send **`--phase done`** with your **result in
   the body** — a concise summary the initiator can use directly, NOT a raw
   dump. If the result would exceed the ~3,000-char body cap, trim it to the
   essentials (or split detail across earlier `--phase context` legs).
3. **`phase:change` from the initiator** — they want a revision; address the
   feedback and re-send **`--phase done`** with the updated result.
4. **`phase:confirm` from the initiator** — your result was accepted; the task
   is closed (todo `completed`). Reconsider your availability (see
   **Availability** below).

**Availability (both flows).** Each peer advertises whether it is accepting work
via its meta `status` (`idle` = open/not working, `available` = working but open,
`busy` = not accepting). The pickers in `/swarm:task` and `/swarm:handover` skip
peers whose `status` is `busy`. **You** own your status — it is a judgment about
willingness, not an automatic toggle. **When you start** a task/handover
(on accept, or at work-start for a handover) reconsider: if taking this on means
you will not accept more, set `busy`; otherwise leave it `idle`/`available`.
**When it closes** (task `confirm`; handover work done; `decline`;
`task_timeout`) reconsider again: if you now have capacity, set it back to
`idle`/`available`. In both cases you **may leave it unchanged** — only merge when
your availability actually changed. Set it with a bare tool call, no prose (like
the todo updates):

```
agent-gossip meta merge --swarm $SWARM --nickname $NICKNAME --merge '{"peers":{"$NICKNAME":{"status":"busy"}}}'
```

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

**Absolute rule (both flows):** every `TodoWrite` call emits **zero**
surrounding prose — no preamble *before* and no postamble *after*. Two
directions: never **announce the upcoming `TodoWrite`** (no "Now I'll track
this in the to-do list", "Let me update the to-do list") and never **report the
transition** afterward (no narration of the auto-confirm like "requested close
— auto-confirming", no outcome line like "💬️ task handed over to …", no
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
`task_progress` (incl. the daemon's keepalive beats) only refreshes the
todo. A `task_timeout` marks the todo `completed` ("timed out").
