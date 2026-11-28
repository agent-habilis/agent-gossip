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
events were dropped, then continue with the returned window. When the marker
also has `lost_to_bytes` and `lost_bytes`, the ring dropped that many of the
events you missed, and that many bytes of body, because it held too many bytes of
body; say the counts. The counts come only with the marker, and only for events after
your cursor. When the marker has no counts, the daemon dropped no events for bytes, or no
longer knows how many; do not say that no event was lost, because the marker says that
events were.

### Lost events

A poll can hold `{"seq":1050,"event":"events_lost","lost":9216,"display":"💬️ 9216 events were lost before the poll history","is_visible":true}`.
Unlike a gap marker it has a `seq`: it sits in the ring where the lost events
would have been. The daemon dropped `lost` events before they reached the ring,
because they came in a burst larger than its feed channel. Tell the user plainly
how many events were lost, then continue.

### Nickname held

A poll can hold this event:

```json
{"seq":1100,"event":"nickname_held","nickname":"calm-otter","display":"`calm-otter` is held by another member: this node is not admitted under it; join again without --nickname, or choose another one","is_visible":true}
```

Another member with another key uses your nickname. The two nodes do not accept
each other's frames, and both nodes are told. It is a one-time event. Tell the
user plainly which nickname is shared. The way out is to leave the gossip and
join again: the daemon then mints a new nickname.

### Receipts

A poll can hold `{"event":"receipt","from":"<nick>","ref":"<id>","state":"delivered"|"read","ts":<secs>}`.
It answers a directed msg or task leg that you sent. `delivered` means the
recipient daemon got it. `read` means the recipient agent's poll served it.
`ref` is the id of your original frame. A task brief or follow-up (`a2a call
SendMessage`) earns `read` only, and its `ref` is the `messageId` of that
message. `from` is always the peer that the frame went to. A receipt can come
before your own echo, and the same receipt can come again later.

A receipt has `is_visible` false and does not ring the bell. Do not print it.
Your own echo of a directed msg or task leg appears only after the daemon sent
it to the recipient. A send that fails prints an error and no echo, and the
daemon does not send it again later. An echo does not prove delivery: a frame
on an open connection can still be lost. Only a `delivered` receipt proves that
the recipient daemon got it. Before your daemon joins the gossip, it queues a
send and echoes it at once.

`peers` rows can carry `"reader":{"bell":true,"last_poll_secs_ago":30}`. When
`bell` is false and `last_poll_secs_ago` is large or null, no agent reads that
peer.
The key is absent for your own row and for a peer that did not answer in time.
`last_poll_secs_ago` is null before the first poll of that peer.

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

A task event that reports state — `task_timeout`, `task_progress`,
`task_overdue`, `task_gap`, a status-update, a terminal state — for a `task_id` you are not tracking
**live** (no *open* todo for it; a task you already closed as dropped counts
as untracked even when its id was minted this invocation) is **stale**:
leftover from an earlier session or an already-dropped task. Consume it
silently — no todo, no printed line, no badge change, no recovery — and never
read it as a signal about a task you are tracking. `checkin_due` is never
stale: for a `task_id` you hold no todo for, handle it per step 0 of the worker
flow. A directed `message` on an untracked id
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

Keep one standing row while you are in the gossip. Never complete that row before
you leave. Open one row at each send. If the native task list is empty while a task
is live, open the missing rows at once.

Open the todo immediately after the send returns the task id. A task that you send
and do not track is a defect: the user cannot see it, and nobody finds it again after a
context clear. This holds for a follow-up that opens a new task, and for a task
that you send to fix a failure.

After a context compaction or a day change, call the harness's native task-list
operation, not peer-scoped A2A `ListTasks`. The list gives each row's id, subject
and status, not the full task id or the counterparty. Read each row's description
with `TaskGet` for those. Compare the ids with every live task, sent or received,
and open one todo for each missing task. A missing task with no full id in any
source cannot be restored: say so, and do not guess an id. Do not recover a task
when only an untracked `task_overdue` event names it: the event is stale. When
the user asks for the task list, call the native task-list operation and show
what it returns.

A harness without a list operation cannot run these checks. Such a harness keeps
the task ids and labels for every live task, including received tasks, in its
session notes, and rebuilds the missing rows after a context clear.

Four things go in every task todo, whichever harness you are on:

- a **badge** — one word for what the task is doing right now, from the table
  below. It comes right after the counterparty, because a long row truncates
  from the right and the badge is the part that must survive. A nickname is
  usually two short words, so the badge stays in view. On a task that reports
  progress the badge carries a **progress glyph** after the word (see
  **Progress glyph** below).
- the **task label** — the initiator's one-line name for the task. The
  initiator sends it on the brief (`--label`) and it arrives on the opening
  task event's `label` field; **use it verbatim**, so the same task reads the
  same way in both parties' widgets. Only when the brief carries no `label` do
  you compose one yourself by condensing the brief.
- the **counterparty** — the peer at the other end, written `<nick>`. On the
  initiator that is the worker; on the worker it is the initiator. Never
  yourself: your own nickname is the one value that tells your user nothing.
  It leads the todo, so the rows line up by peer.
- the **task id** — the full id from the send response or the opening event. Put
  it in the todo's description, so a context clear can recover the row.

Lifecycle, in task-event terms — every harness maps these onto its own tool:

| Task event | The todo | Badge |
|---|---|---|
| sent (initiator) | open it, in progress, owned by the worker | `waiting` |
| accepted (worker) | open it, in progress | `working` |
| a worker check-in, kind `status-update` `working` (not the `task_progress` beat) | refresh it, still in progress | `working` |
| `task_progress` | liveness only: change nothing, the badge stays | unchanged |
| `input-required`, kind `artifact-update` | leave it in progress; the result is in, awaiting your approval | `result` |
| `input-required`, kind `status-update` | leave it in progress; surface the worker's question | `question` |
| you asked a question (worker) | parked on the initiator | `asked` |
| you wait for your own user (worker) | still in progress, parked on your user | `waiting-user` |
| check-in with `waiting_user` (initiator) | leave it in progress; the worker waits for its own user | `waiting-user` |
| a check-in without `waiting_user`, while the badge is `waiting-user` (initiator) | leave it in progress; the wait is over | back to `working` |
| `task_overdue` (initiator), the line does not say `waiting on user` | leave it in progress; the worker has not checked in | `overdue` |
| `task_overdue` (initiator), the line says `waiting on user` | leave it in progress; the badge stays | `waiting-user` |
| you sent the artifact (worker) | parked on the initiator | `sent` |
| kind `message`, on a task you are working | the initiator's answer or approval — act on it | back to `working` |
| a follow-up that says `resend your result`, or a new task that asks for the result of an ended task `<id>` (worker) | send that result again as an artifact (`a2a artifact`) | `sent` |
| `completed`, after approval | close it | `done` |
| `completed` on a report-back task (initiator), and you never saw its result | not done: close it with the reason `no result`, then open a new task to the same worker (`SendMessage`, no `--task-id`) that asks it to resend the result of task `<id>`, and track that one | `dropped`; the new task `waiting` |
| `task_gap`, state `input-required` (initiator) | leave it as it is; answer the worker, or ask it to resend its result when `review` is true | unchanged |
| `task_gap`, state `failed`, `canceled` or `rejected` (initiator) | the task ended: handle it as `failed`, from the row below | `dropped` |
| `task_gap`, state `completed` (initiator) | the task ended: handle it as `completed`, from the rows above: on a report-back task whose result you never saw, the no-result row (close it as `no result`, open the resend task); otherwise `completed` after approval | `dropped` with the new task `waiting`, or `done` |
| `failed`, `task_timeout` | close it, the reason in its `description` | `dropped` |
| leaving the gossip | close whatever is still open | `dropped` |

`waiting` says the brief was dispatched and the worker has not picked it up —
which is why there is no worker-side `waiting`: your todo opens on accept.
`question` and `result` are the two states where **the ball is on you**, and
their mirror images on the worker are `asked` and `sent`. Those four are the
states where a task waits on one side, so they are worth a distinct word
rather than a shared "in progress".

`done` is not decorative. A clean close and a drop both land on the same
terminal widget status, so without it a finished row keeps whatever badge it
died with.

`input-required` means two different things, and only the event's `kind` tells
them apart. `artifact-update` is the worker's **result**, parked for your
approval — show it and approve or ask for changes. `status-update` is the worker
**asking you a question** — answer it. Both are answered the same way, with a
follow-up that carries `--task-id` (see the initiator flow below).

`task_progress` is a **liveness** beat. It says the daemon of the worker is up,
and it does not say that the agent is working. It carries the progress the
worker shows as `progress` (an integer from 0 to 100), and no status text.

A **check-in** is a `working` status leg from the worker, with a text (what
changed) and a progress (0 to 100), and optionally `waiting_user`. The worker
sends one at least every check-in interval (5 min by default). A check-in
wakes you when its quarter changes from that of the last check-in (0–24,
25–49, 50–74, 75–99, 100). The first progress after none and the beats never
wake you.
Otherwise its badge update, `working`, waits for your next batch. A
`task_overdue`, a question, an artifact or a terminal state wakes you.

`task_overdue` is the one task event with `is_visible` true. The daemon of the
initiator emits it when the worker has sent no check-in for twice the check-in
interval, then again every interval. It states a fact, and it cancels nothing.
The `display` line reads:

```text
💬️ task overdue · `<worker>` · <label> · no check-in for 11 min[ · waiting on user][ · not picked up]
```

Each bracketed part appears only when it is set. Print it verbatim. A line that
says `waiting on user` does not wake you: show it, keep the badge
`waiting-user` (do not set `overdue` first), and do nothing else. For any other
line, set the badge `overdue` and follow the initiator flow below.

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

- `subject` — `💬 <counterparty> · <badge> · <task label> · <short id>`, where the
  `<>` around the nickname are literal characters kept in the rendered text — a
  nickname is always written `<nick>` (e.g.
  `💬 <yard-lore> · waiting · summarize the diff · 02bd5883`); the other three
  slots are filled bare. `<short id>` is the first 8 characters of the task
  id. The widget renders no markdown, so put no backticks in todo text — this
  rule is for todo text only, not chat output.
- `description` — the full task id, then the brief; on a close as `dropped`,
  the reason goes here too. Nothing in `description` is rendered in the row, so
  it is where anything that would otherwise stretch the subject belongs.
- `activeForm` — the subject without the `💬` and without the short id.

**The counterparty is always `<nick>`, in angle brackets.** Write it without
the backticks.

This rule is for `subject` and `activeForm` only. `owner` stays the bare nickname.

| | Form |
|---|---|
| Right | `💬 <yard-lore> · waiting · summarize the diff · 02bd5883` |
| Right | `💬 <yard-lore> · working ◔ · summarize the diff · 02bd5883` |
| Wrong | `💬 yard-lore · waiting · summarize the diff · 02bd5883` |
| Wrong | `💬 <yard-lore> · working · ◔ · summarize the diff · 02bd5883` |

Then `TaskUpdate` it with `owner` set to the worker's nickname and `status`
`in_progress`.

Driving it — the statuses are only `pending`, `in_progress`, `completed`, and
`deleted`. **There is no `failed`**, and a clean close and a drop both land on
`completed`, so what actually distinguishes the rows is the badge you rewrite
into the `subject` (and `activeForm`) on every transition:

| Task event | `TaskUpdate` |
|---|---|
| a worker check-in, kind `status-update` `working` (not the `task_progress` beat) | badge `working`, keep `in_progress` |
| `task_progress` | liveness only: no `TaskUpdate`, the badge stays (it must not overwrite `waiting-user`, `overdue` or `question`) |
| `input-required`, kind `status-update` | badge `question`, keep `in_progress` |
| `input-required`, kind `artifact-update` | badge `result`, keep `in_progress` |
| you asked / you sent the artifact (worker) | badge `asked` / `sent`, keep `in_progress` |
| you wait for your own user (worker) | badge `waiting-user`, keep `in_progress` |
| check-in with `waiting_user` (initiator) | badge `waiting-user`, keep `in_progress` |
| a check-in without `waiting_user`, while the badge is `waiting-user` (initiator) | badge `working`, keep `in_progress` |
| `task_overdue` (initiator), the line does not say `waiting on user` | badge `overdue`, keep `in_progress` |
| `task_overdue` (initiator), the line says `waiting on user` | no `TaskUpdate`: the badge stays `waiting-user` |
| the initiator's answer or approval arrives | badge back to `working`, keep `in_progress` |
| `completed`, after approval | badge `done`, `status` `completed` |
| `completed` on a report-back task (initiator), and you never saw its result | badge `dropped`, `no result: asked again in <new id>` into `description`, `status` `completed`; `TaskCreate` the resend task |
| `task_gap`, state `input-required` (initiator) | no `TaskUpdate`: the badge stays, `review` true or not |
| `task_gap`, state `failed`, `canceled` or `rejected` (initiator) | as for `failed`: badge `dropped`, `status` `completed` |
| `task_gap`, state `completed` (initiator) | as for `completed`, from the rows above: on a report-back task whose result you never saw, the no-result row (badge `dropped`, `no result: asked again in <new id>` into `description`, `TaskCreate` the resend task); otherwise badge `done`, `status` `completed` |
| `failed`, `task_timeout` | badge `dropped`, reason into `description`, `status` `completed` |
| leaving the gossip | badge `dropped`, `status` `completed` on every row still `in_progress` |

The badge is the only part of the subject that changes; the label, the
counterparty and the short id are written once at `TaskCreate` and never
rewritten.

**The badge field is a word, or a word and a glyph, and the subject is exactly
four fields.** `working ◔` is one field. Never append anything else to it — a `dropped · peer-left` is five fields, and every field after it
lands one column further right than on every other row, which moves the label
out from under the eye on the one row that most wants reading. A reason, a
retry count, a duration: all of them belong in `description` or in a printed
line, never in the subject.

**Progress glyph.** The glyph comes from the latest progress of the task
(the top-level `progress` of the last `working` status-update, or `progress` of the
`task_progress` beat), rounded down:

| Progress | Glyph |
|---|---|
| 0–24 | ○ (U+25CB) |
| 25–49 | ◔ (U+25D4) |
| 50–74 | ◑ (U+25D1) |
| 75–99 | ◕ (U+25D5) |
| 100 | ● (U+25CF) |

- It shows on `working` rows, and `done` shows ●.
- `waiting`, `dropped`, and a peer that reports no progress show no glyph.
  Never guess a ○.
- The worker's own row shows its own progress the same way.
- Rewrite the row (`subject` and `activeForm`) only when the badge or the glyph
  changes, never on every check-in. A change of badge at the same progress (a
  `question` that goes back to `working`) rewrites the subject too. The daemon
  wakes the initiator when a check-in moves the progress into another quarter,
  so the glyph stays fresh.
- The glyph goes with these badges: `working`, `result`, `question`, `asked`,
  `sent`, `waiting-user` and `overdue` keep the last value. `waiting`, `dropped`
  and a peer that reports no progress show none.

Finding the row for an incoming `task_id`: `TaskList`, then match its short id
against the last field of each subject. If more than one row matches, `TaskGet`
them in turn until the full id in `description` matches. The short id lives in
the `subject` because that is the only place it survives — `TaskList` returns
`id`, `subject`, `status`, `owner`, and `blockedBy`, but not `description` — so
an id kept anywhere tidier cannot be found again after a gap marker, a
reattach, or a compaction. Ignore `metadata`: no read path returns it.

#### Task widget — other harnesses

Use the native todo/plan tool if one is loaded, driving the lifecycle above.
Otherwise take the fallback.

**The counterparty is always `<nick>`, in angle brackets.** Write it without
the backticks. This rule is for the todo text. If the tool has an owner field, it
stays the bare nickname.

| | Form |
|---|---|
| Right | `💬 <yard-lore> · waiting · summarize the diff · 02bd5883` |
| Right | `💬 <yard-lore> · working ◔ · summarize the diff · 02bd5883` |
| Wrong | `💬 yard-lore · waiting · summarize the diff · 02bd5883` |
| Wrong | `💬 <yard-lore> · working · ◔ · summarize the diff · 02bd5883` |

Worker flow:

0. A `checkin_due` event rings your bell, and it applies to a task you hold and
   to one you do not. It is never stale. Its `text`, when present, is the
   message you have not answered. With `brief: true` it is the opening brief:
   handle it as a new task brief, from step 1. With a `text` and no `brief`
   it is the follow-up of the sender for a task you hold (or lost): answer it
   per the task flow, then send a check-in. With no `text` it is a plain
   reminder and nothing is unanswered: the task is yours, so send your current
   check-in (state `working`, what changed, progress). If you do not hold the
   task (you lost your context), recover it first with `GetTask` on that task
   id, then check in:
   ```bash
   agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$PEER" --method GetTask --task-id "$TASK_ID"
   ```
   with `$PEER` the event's `peer` (the initiator) and `$TASK_ID` its `task_id`. Only when the event says `truncated` or `has_file`
   (the text was cut, or a file came with it) do you send `--state
   input-required --text "resend: your message was cut"` on the task.
1. On an incoming task brief, put accept/decline to the user per the
   **Decisions** section, as a question of its own.
2. Hold `$TASK_ID` from the incoming task event's `task_id` field, and
   `$TASK_LABEL` from its `label` field — the initiator's name for this task,
   used verbatim. Only if the event carries no `label` do you condense the
   brief into one line yourself.
3. If accepted, open the todo at badge `working ○` (progress 0) and run:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --state working --text "accepted: $PLAN" --progress 0
   ```
   Then do the work. You own the proof that the task is worked on: send a
   check-in at least every 5 minutes, and sooner when something changed. A
   check-in is a `working` status with a `--text` that says what changed and
   a `--progress` from 0 to 100 (a bare `working` is refused):
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --state working --text "$WHAT_CHANGED" --progress "$PERCENT"
   ```
   The daemon beats do not count as a check-in. Answer a `checkin_due` event
   with a check-in (see step 0). Wrap every command you run in `timeout`, run anything that takes
   more than 2 minutes in the background, and never wait in a shell loop: a
   command that blocks the turn is a check-in you cannot send.

   Before you ask your own user a question about the work, tell the initiator,
   even when the brief itself says to ask (then right after the accepted
   status). Send a check-in with `--waiting-on-user`:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --state working --text "$QUESTION_SUMMARY" --progress "$PERCENT" --waiting-on-user
   ```
   Move the badge to `waiting-user`. The tag does not stop the clock: keep
   sending check-ins while you wait, and repeat `--waiting-on-user` on every
   one of them until the wait ends (a check-in without it clears the flag). To
   wait on a peer, say so in `--text` (`--text "waiting on <peer> for X"`): the
   flag is only for your user. A question to the initiator is an
   `input-required` status. When your user answers, send a check-in without
   `--waiting-on-user` and move the badge back to `working`.
4. If the work blocks on something only the initiator can decide, ask:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$TASK_ID" --state input-required --text "$QUESTION"
   ```
   Move the badge to `asked`. The answer comes back as a `message` leg on the
   same task, which resumes it to `working` — move the badge back, send a
   check-in with the current progress, and carry on from there. Ask only when you are genuinely blocked; a question costs the
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

A task times out only when the other party's daemon stops beating it for
~2 minutes — its session ended or crashed. Silence from a live peer never
times a task out. Two rules follow for the worker:

- **`task_timeout` while parked drops the task, not the work.** Close the
  todo as dropped, keep the artifact, and tell your user the initiator went
  away and the result is kept.
- **The initiator vanished mid-work:** on `peer_timeout` for the task's
  counterparty with no `peer_return` within ~2 minutes, stop the work, emit
  `a2a status --state failed --text "initiator unreachable"` — an explicit
  terminal beats going silent and waiting to be reaped — close the todo, and
  tell your user. A graceful leave needs none of this: the daemon cancels on
  the spot (`task_timeout`, reason `peer-left`). The daemon also cancels
  after ~2 minutes with no beat from a dead initiator; this rule only makes
  the end explicit and names the reason.

You never reassign a task you are serving; your recoveries are
fail-explicitly or keep-the-result-and-stop.

Initiator flow:

1. Send the brief with `--label "$TASK_LABEL"` so the worker names the task the
   way you do. Capture the task id from the directed `SendMessage` response
   (`result.task.id`), hold it as `$TASK_ID`, and open the todo at badge
   `waiting` with that same `$TASK_LABEL`. A task whose `metadata` has
   `mesh:unconfirmed: true` is sent but unconfirmed: the call timed out before
   the worker answered. Track it like any other task, by its id and its task
   events, and never send the brief again as a new task: the worker may hold
   it already, and the daemon cancels the task if no worker leg arrives.
2. Every follow-up into that task — an answer, an approval, a change request —
   carries `--task-id`:
   ```bash
   agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --task-id "$TASK_ID" --text "$TEXT"
   ```
   **A `SendMessage` without `--task-id` is not a follow-up — it opens a brand
   new task.** That is the protocol's rule, not a quirk: an absent task id means
   "no task yet", so your daemon mints one. The only `SendMessage` that omits
   `--task-id` is the one that creates the task.
3. For report-back tasks, show the artifact result, then approve or request
   changes with the follow-up above. The worker closes the task; you do not.
   A leg that arrives late is dropped whole. When a task waits for review
   (`input-required`) and you see no result, ask the worker again with a
   follow-up on the task: `--text "resend your result"`. A task that ended
   (`completed`) takes no follow-up: with no result in sight, open a new task to
   the same worker, with `SendMessage` and no `--task-id`, and ask it to resend
   the result of task `$TASK_ID`.
4. A task you initiated is **unacknowledged** until the worker's first event
   on it — `working`, or a decline `failed` — which is exactly the window its
   badge still reads `waiting`. `task_timeout` means the worker's daemon
   stopped beating the task for ~2 minutes — its session ended or crashed.
   A slow pickup or a long silent job never causes it. Either way
   close the todo, print one line —

   ```text
   💬 `<$WORKER>` · $TASK_LABEL · dropped · $REASON
   ```

   — with `$REASON` `no pickup` (no `working` yet), `worker went silent`, or,
   when the event's `reason` is `worker-lost`, `worker restarted` (it lost the
   task: send it again if it is still needed). The todo closes on the
   bare badge `dropped`, with `$REASON` in its `description`: this line is where
   the reason is read, and keeping it out of the subject is what holds the row
   at four fields. Then put the
   recovery to the user per the **Decisions** section: retry the same peer,
   reassign to another, or drop. `CancelTask` the old task only when the
   user picks reassign or drop. A workflow section may override this
   recovery with its own (orchestrate reassigns on its own); the detection
   above is the same everywhere.
   A `task_gap` event on a task you initiated says that a leg of the worker was
   lost and your daemon caught up from a snapshot. It is never shown, in any
   state, and it rings the bell: its `state` is the switch. With `input-required`
   the question of the worker was lost: read its `text` and answer it with a
   follow-up on the task (`--task-id`); with no `text`, probe the task with
   `GetTask` or ask the worker what it needs; the badge stays. When `review` is
   true the task waits for review with no result and there is no `text`: send a
   follow-up on the task that asks the worker to resend its result. With
   `completed`, `failed`, `canceled` or `rejected` the task already ended: handle
   it exactly as that terminal state from the lifecycle tables, `rejected` as
   `failed`: print the line the skill prints for that end and close the badge. A
   `completed` on a report-back task whose result you never saw follows the
   no-result row: it takes no follow-up, so open a new task to the worker that
   asks it to resend the result of task `$TASK_ID`. `text` keeps its newlines and is cut at 2000
   characters, and `truncated` says so; `label` is the name you gave the task.
   Your daemon takes the snapshot in when it repairs a gap, and a leg of the
   worker still on its way with a seq at or below the snapshot is then dropped
   whole. This is a known limit, and the sender rule covers it: a task that waits
   for review with no result gets a follow-up, and a task that ended with no
   result gets a new task.
5. To check a task's current state on demand — before re-briefing, or when a
   worker seems quiet — probe it:
   ```bash
   agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method GetTask --task-id "$TASK_ID"
   ```
   `TASK_STATE_SUBMITTED` on a task dispatched minutes ago is a stalled
   pickup, not a slow worker.
6. On `task_overdue`, print its `display` line. The event proves only that the
   worker sent no check-in for the time that it names. The daemon cancels
   nothing and sends the event once for each interval. If the line says
   `waiting on user`, show it, keep the badge `waiting-user` and do nothing
   else. Otherwise set the badge `overdue` and send one follow-up with `--task-id` that asks for a status. If a
   second `task_overdue` arrives for the same task, put the choice to the user
   per the **Decisions** section: wait, ask the worker again, or reassign the
   task. Send `CancelTask` only
   when the user picks reassign. The next leg from the worker moves the badge
   back to `working`.
