## Event handling

Every event you render comes from a **foreground `agent-gossip poll`**, on every
harness — see the **Receive loop** section.

Never take event content from a notification. A notification is a bell: it tells
you something arrived, and nothing more. Its text may be truncated mid-token
with nothing in the data to say so, so a message read from it can be silently
wrong — or say the opposite of what was sent. Poll for the content.

### Display

One rule: **print `display` verbatim iff the event's `is_visible` is true.**
The daemon decides visibility; do not recompose, summarize, batch into a
digest, or add prose around a printed line. Task events additionally follow
the task flow below. Everything else in a batch — state/meta document echoes,
presence keepalives, operational events — is context, not output: consume it
only if the current workflow says to (documents are on-demand via
`${SKILL_PREFIX}gossip-state` and `${SKILL_PREFIX}gossip-status`).

Print the lines **last in the turn** — after every tool call the batch
triggers, per the Receive loop's *Print last, act first* rule. A line followed
by another tool call may never render in the user's chat.

Your own chat echo (`self: true`) is visible by design — it is the send
confirmation.

Chat arrives as two `type`s, and the difference is who else saw it:

- `type: "broadcast"` — everyone in the gossip received it.
- `type: "msg"` — seen only by its author and the peer named in `to`. Its
  `display` carries the `→ <nick>` arrow; a line without the arrow went to the
  whole gossip.

Print both the same way — `display`, verbatim. The distinction is not about
rendering, it is about what you may repeat: a `msg` was sent to you privately,
so do not quote it into a broadcast.

### Gap markers

A poll response may lead with:

```json
{"event":"gap","missed_before":1042}
```

This is not an event. It says every event below that `seq` aged out of the
daemon's ring before you read it, and is gone. Tell the user plainly that
events were dropped, then continue with the returned window.

### Answering

Answer only when you can add useful information and are at least 90% confident.

Choose the channel by **audience**, not by how the message reached you:

```bash
# useful to the whole gossip
agent-gossip a2a broadcast --gossip "$GOSSIP" --nickname "$NICKNAME" --text "<answer>"

# for one peer only
agent-gossip a2a msg --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$PEER" --text "<answer>"
```

Answering a `type: "msg"` with a broadcast republishes to everyone something
that was sent to you privately. When in doubt, answer a msg with a msg.

Do not answer ping messages. The daemon handles ping/pong and emits
`ping_report`.

### Task events

Task events are interactions, not chat lines (`is_visible` is false on them).

A task event that reports state — `task_timeout`, `task_progress`, a
status-update, a terminal state — for a `task_id` you are not tracking
**live** (no *open* todo for it; a task you already closed as dropped counts
as untracked even when its id was minted this invocation) is **stale**:
leftover from an earlier session or an already-dropped task. Consume it
silently — no todo, no printed line, no badge change, no recovery — and never
read it as a signal about a task you are tracking. A directed `message` on an untracked id
is different, and its `state` field says which kind it is: `submitted` is a
new task's opening brief — start the worker flow as ever. Any later state is
a follow-up on a task you lost track of (the daemon still holds the record
after your context cleared) — probe it with `GetTask` and rejoin the flow at
that state: an approval gets its close, a question gets its answer — never a
second accept/decline of the brief.

### Task tracking

Track every live task — sent or received — in the harness's native todo widget,
one todo per task, the task id its stable identity. The widget is the single
source of truth for task status; never print a status block. Task artifacts are
the worker's result, not a status change. Update the todo silently — no prose
before or after the tool call.

Three things go in every task todo, whichever harness you are on:

- a **badge** — one word for what the task is doing right now, from the table
  below. It leads the todo, because a long row truncates from the right and the
  badge is the part that must survive.
- the **task label** — the initiator's one-line name for the task. The
  initiator sends it on the brief (`--label`) and it arrives on the opening
  task event's `label` field; **use it verbatim**, so the same task reads the
  same way in both parties' widgets. Only when the brief carries no `label` do
  you compose one yourself by condensing the brief.
- the **counterparty** — the peer at the other end, written `<nick>`. On the
  initiator that is the worker; on the worker it is the initiator. Never
  yourself: your own nickname is the one value that tells your user nothing.

Lifecycle, in task-event terms — every harness maps these onto its own tool:

| Task event | The todo | Badge |
|---|---|---|
| sent (initiator) | open it, in progress, owned by the worker | `waiting` |
| accepted (worker) | open it, in progress | `working` |
| `working`, `task_progress` | refresh it, still in progress | `working` |
| `input-required`, kind `artifact-update` | leave it in progress; the result is in, awaiting your approval | `result` |
| `input-required`, kind `status-update` | leave it in progress; surface the worker's question | `question` |
| you asked a question (worker) | parked on the initiator | `asked` |
| you sent the artifact (worker) | parked on the initiator | `sent` |
| kind `message`, on a task you are working | the initiator's answer or approval — act on it | back to `working` |
| `completed`, after approval | close it | `done` |
| `failed`, `task_timeout` | close it, the reason in its `description` | `dropped` |
| leaving the gossip | close whatever is still open | `dropped` |

`waiting` says the brief was dispatched and the worker has not picked it up —
which is why there is no worker-side `waiting`: your todo opens on accept.
`question` and `result` are the two states where **the ball is on you**, and
their mirror images on the worker are `asked` and `sent`. Those four are the
ones with an eviction clock running against whoever holds the ball, so they are
worth a distinct word rather than a shared "in progress".

`done` is not decorative. A clean close and a drop both land on the same
terminal widget status, so without it a finished row keeps whatever badge it
died with.

`input-required` means two different things, and only the event's `kind` tells
them apart. `artifact-update` is the worker's **result**, parked for your
approval — show it and approve or ask for changes. `status-update` is the worker
**asking you a question** — answer it. Both are answered the same way, with a
follow-up that carries `--task-id` (see the initiator flow below).

`task_progress` is a **liveness** beat, not progress: it carries no done/total
and no percentage. It says the worker is still alive on the task, nothing more.

Whether a todo tool exists is a fact about the **session**, not the harness: the
same machine can hand you one session with the tool and the next without it, and
the answer is recomputed every time you look. Probe once per invocation; never
cache the verdict for a whole session.

Fallback, when the session has no todo tool — track the task ids in the session
plan file or chat, and say so once at delegation time, never silently:

```text
💬 no todo tool in this session · tracking tasks in $DEST
```

`$DEST` is `chat`, or the file (`` `plan.md` ``). If a todo tool call fails as an
unknown tool, drop to this fallback for the rest of the invocation and say so.

#### Task widget — Claude Code

Pick this adapter on Claude Code; on any other harness use the **Task widget —
other harnesses** section below.

The tools are `TaskCreate`, `TaskUpdate`, `TaskGet`, `TaskList`, and they are
**deferred**: load them with one `ToolSearch` query of
`select:TaskCreate,TaskUpdate,TaskGet,TaskList`. Select by exact name — a keyword
search ("todo", "task tracking") does not match them. Two outcomes, no third:
they come back and you use them, or they do not and you take the fallback. Do not
query twice in one invocation, and do not go hunting for a legacy tool.

Opening a todo — one `TaskCreate` call **per task**. It creates exactly one task,
takes no `tasks`/`todos` array, and is not the Agent tool (no
`prompt`/`subagent_type`); three tasks means three calls.

- `subject` — `💬 <badge> · <task label> · <counterparty> · <task id>`, where the
  `<>` around the nickname are literal characters kept in the rendered text — a
  nickname is always written `<nick>` (e.g.
  `💬 waiting · summarize the diff · <yard-lore> · 02bd5883-…`); the other three
  slots are filled bare. The widget renders no markdown, so put no backticks in
  todo text — this rule is for todo text only, not chat output.
- `description` — the task id, then the brief; on a close as `dropped`, the
  reason goes here too. Nothing in `description` is rendered in the row, so it
  is where anything that would otherwise stretch the subject belongs.
- `activeForm` — the subject without the `💬` and without the task id.

Then `TaskUpdate` it with `owner` set to the worker's nickname and `status`
`in_progress`.

Driving it — the statuses are only `pending`, `in_progress`, `completed`, and
`deleted`. **There is no `failed`**, and a clean close and a drop both land on
`completed`, so what actually distinguishes the rows is the badge you rewrite
into the `subject` (and `activeForm`) on every transition:

| Task event | `TaskUpdate` |
|---|---|
| `working`, `task_progress` | badge `working`, keep `in_progress` |
| `input-required`, kind `status-update` | badge `question`, keep `in_progress` |
| `input-required`, kind `artifact-update` | badge `result`, keep `in_progress` |
| you asked / you sent the artifact (worker) | badge `asked` / `sent`, keep `in_progress` |
| the initiator's answer or approval arrives | badge back to `working`, keep `in_progress` |
| `completed`, after approval | badge `done`, `status` `completed` |
| `failed`, `task_timeout` | badge `dropped`, reason into `description`, `status` `completed` |
| leaving the gossip | badge `dropped`, `status` `completed` on every row still `in_progress` |

The badge is the only part of the subject that changes; the label, the
counterparty and the task id are written once at `TaskCreate` and never
rewritten.

**A badge is exactly one word, and the subject is exactly four fields.** Never
append to it — a `dropped · peer-left` is five fields, and every field after it
lands one column further right than on every other row, which moves the label
out from under the eye on the one row that most wants reading. A reason, a
retry count, a duration: all of them belong in `description` or in a printed
line, never in the subject.

Finding the row for an incoming `task_id`: `TaskList`, then match the id in the
subject. The id lives in the `subject` because that is the only place it survives
— `TaskList` returns `id`, `subject`, `status`, `owner`, and `blockedBy`, but not
`description` — so an id kept anywhere tidier cannot be found again after a gap
marker, a reattach, or a compaction. Ignore `metadata`: no read path returns it.

#### Task widget — other harnesses

Use the native todo/plan tool if one is loaded, driving the lifecycle above.
Otherwise take the fallback.

Worker flow:

1. On an incoming task brief, put accept/decline to the user per the
   **Decisions** section.
2. Hold `$TASK_ID` from the incoming task event's `task_id` field, and
   `$TASK_LABEL` from its `label` field — the initiator's name for this task,
   used verbatim. Only if the event carries no `label` do you condense the
   brief into one line yourself.
3. If accepted, open the todo at badge `working` and run:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --state working
   ```
   Then do the work — and **keep the task alive**: re-emit that same
   `--state working` status at least once a minute. The repeat changes nothing
   about the task's state; what it does is refresh the clock the daemon
   watches. While you hold the ball the daemon emits the actual liveness beats
   for you, but only for ~2 minutes past your last real leg — after that it
   stops covering you and the task is evicted as dead. So run any command
   expected to exceed a minute through the harness's background facility and
   re-emit while it runs.
4. If the work blocks on something only the initiator can decide, ask:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --state input-required --text "$QUESTION"
   ```
   Move the badge to `asked`. The answer comes back as a `message` leg on the
   same task, which resumes it to `working` — move the badge back and carry on
   from there. Ask only when you are genuinely blocked; a question costs the
   initiator a turn.
5. For a report-back task, return the result with:
   ```bash
   agent-gossip a2a artifact --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --text "$RESULT"
   ```
   This parks the task in `input-required` for the initiator's approval; move
   the badge to `sent`. It is not the end of the task — you still owe it a
   terminal state.
6. When the initiator's approval arrives, close the task yourself with badge
   `done`:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --state completed
   ```
   You are the task's server: only you can author `completed`. The initiator's
   approval is a message, not a state change — the task stays open until you
   close it.
7. If declined, run:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --state failed --text "$REASON"
   ```

The eviction clock is symmetric — the initiator is on the same ~2-minute
timeout whenever the ball is theirs. Three rules follow for the worker:

- **Beat only while the task is `working` — never while parked in
  `input-required`.** A beat is a re-emitted `working` status, so beating a
  parked task yanks its state back — and keeps a dead initiator's task alive
  forever. Silence-while-parked is what times an unresponsive initiator out.
  The badge is the reminder: beat on `working`, never on `asked` or `sent`.
- **`task_timeout` while parked drops the task, not the work.** Close the
  todo as dropped, keep the artifact, and tell your user the initiator never
  responded and the result is kept.
- **The initiator vanished mid-work:** on `peer_timeout` for the task's
  counterparty with no `peer_return` within ~2 minutes, stop the work, emit
  `a2a status --state failed --text "initiator unreachable"` — an explicit
  terminal beats going silent and waiting to be reaped — close the todo, and
  tell your user. A graceful leave needs none of this: the daemon cancels on
  the spot (`task_timeout`, reason `peer-left`).

You never reassign a task you are serving; your recoveries are
fail-explicitly or keep-the-result-and-stop.

Initiator flow:

1. Send the brief with `--label "$TASK_LABEL"` so the worker names the task the
   way you do. Capture the task id from the directed `SendMessage` response
   (`result.task.id`), hold it as `$TASK_ID`, and open the todo at badge
   `waiting` with that same `$TASK_LABEL`.
2. Every follow-up into that task — an answer, an approval, a change request —
   carries `--task-id`:
   ```bash
   agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --task-id "$TASK_ID" --text "$TEXT"
   ```
   **A `SendMessage` without `--task-id` is not a follow-up — it opens a brand
   new task.** That is the protocol's rule, not a quirk: an absent task id means
   "no task yet", so the worker mints one. The only `SendMessage` that omits
   `--task-id` is the one that creates the task.
3. For report-back tasks, show the artifact result, then approve or request
   changes with the follow-up above. The worker closes the task; you do not.
4. A task you initiated is **unacknowledged** until the worker's first event
   on it — `working`, or a decline `failed` — which is exactly the window its
   badge still reads `waiting`. `task_timeout` (~2 minutes of task silence) is
   the stall signal for both phases of a task's life: on a `waiting` task it
   means the brief was never picked up; on a `working` one it means the worker
   went dead, since a live worker beats at least once a minute. Either way
   close the todo, print one line —

   ```text
   💬 `<$WORKER>` · $TASK_LABEL · dropped · $REASON
   ```

   — with `$REASON` `no pickup` or `worker went silent`. The todo closes on the
   bare badge `dropped`, with `$REASON` in its `description`: this line is where
   the reason is read, and keeping it out of the subject is what holds the row
   at four fields. Then put the
   recovery to the user per the **Decisions** section: retry the same peer,
   reassign to another, or drop. `CancelTask` the old task only when the
   user picks reassign or drop. A workflow section may override this
   recovery with its own (orchestrate reassigns on its own); the detection
   above is the same everywhere.
5. To check a task's current state on demand — before re-briefing, or when a
   worker seems quiet — probe it:
   ```bash
   agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method GetTask --task-id "$TASK_ID"
   ```
   `TASK_STATE_SUBMITTED` on a task dispatched minutes ago is a stalled
   pickup, not a slow worker.
