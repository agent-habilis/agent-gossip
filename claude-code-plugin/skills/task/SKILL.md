---
name: task
description: Send one or more tasks to peers and get their results back. Use when the user wants other agents to run work and report back (e.g. review src/net on one peer, review src/daemon on another). Task-first - $ARGUMENTS is the task(s) to send (else the current plan); each task has its own completion criteria and worker, is tracked in the to-do list, and its result prints when the worker finishes. Re-invoke to add more tasks.
---

## What this does

Sends **one or more tasks** to other participants and collects each result.
Each task is one **A2A task** on the swarm: a directed `SendMessage` creates
it (the worker mints the task id and returns the `Task`); the worker **runs the
work and returns its result** as an `artifact`, and you **approve** it (or ask
for a change), after which the worker marks it `completed`. That is the
difference from `/gossip:handover`, which hands a task off and walks away with no
result.

Every task is **independent**: its own `task_id`, its own worker, its own
clear **completion criteria**, its own to-do entry. There is **no** group-level
outcome — when a worker finishes, its result prints and that task closes. If
you later want something done across the results, ask in a normal turn; this
skill does not encode any cross-task step.

You can send several tasks at once, and you can **re-invoke `/gossip:task`** to
add more later — new tasks append to the same to-do list.

## Silent execution

Run the whole skill **silently**. Do NOT narrate steps, echo variables (ids,
the worker list), print commands or their output, or announce what you are
about to do. The **only** things that ever appear are: the not-in-swarm guard
line (when it applies), **plan mode** (the drafted tasks), the native
**`TodoWrite`** to-do list, and **each worker's returned result** when it
finishes.

**Absolute rule (not a blocklist):** a silent step emits its tool call and
**zero** surrounding prose — no preamble *before* it and no postamble *after*
it. This is two-directional: never **announce an upcoming action** ("Now I'll
track this in the to-do list", "Let me add this to the to-do list") and never
**report a completed transition** (a parenthetical aside, or a line narrating
having stayed silent, e.g. `(worker confirmed — todo marked completed)`). The
`TodoWrite` call simply happens. A worker's **returned result** is the
deliverable of that task, not a transition — printing it is fine; announcing or
narrating the `TodoWrite` is not.

## Pre-flight: guard

If you hold `$SWARM`/`$NICKNAME` from a `/gossip:create` or `/gossip:join`
`ready` event this session, proceed. Otherwise try to reattach first:
follow `../shared/reattach.md` (resolved relative to this SKILL.md's
directory). Only if reattach also yields no swarm, print:
```
💬 Not in a swarm. Use /gossip:create or /gossip:join first.
```
and STOP.

## The task(s)

Establish *what* is being sent **before** choosing who runs it:

- **If `$ARGUMENTS` is non-empty**, it **is** the task spec. It may describe a
  single task or several distinct ones (e.g. `/gossip:task review src/net on one
  peer, review src/daemon on another` ⇒ two tasks).
- **Otherwise**, the task is your current conversation/plan (one task).

## Read the roster

Read the live roster (connectivity) and the **meta** channel (what each peer
self-reports it runs on — the binary does not carry it) — silently, don't print
either:

```bash
agent-gossip peers --swarm "$SWARM" --nickname "$NICKNAME"
agent-gossip meta get --swarm "$SWARM" --nickname "$NICKNAME"
```

`peers` returns
`{"ok":true,"participants":[{"nickname","last_seen_secs_ago","quiet","reach"}…],"participant_count":N}`;
`meta get` returns `{"ok":true,"document":{"peers":{"<nick>":{"model","harness","host","status"}…}},…}`.
Look up each peer's `model`/`harness`/`host`/`status` by nickname in
`document.peers` (absent ⇒ unreported). Drop any entry with `"quiet":true`,
**and any whose `document.peers[<nick>].status` is `"busy"`** (that peer is not
accepting work; `idle`/`available`/absent all stay eligible). Rank the rest by
availability then recency: `idle` ahead of `available` ahead of unreported, and
within each by `last_seen_secs_ago` ascending (most recently active first). If
there are no eligible peers, print `💬️ no available peers to send tasks to` and
STOP.

## Enter plan mode and build the tasks

Go **straight into plan mode**: as the first action after reading the roster,
**call the `EnterPlanMode` tool**.

Then, **inside plan mode** and silently, draft the plan. For **each** task lay
out a brief the worker can act on, with an explicit completion criteria block, and a
**proposed worker**:
```
### Task N — <one-line summary>   (→ worker: <nick>)
## Task
<what to do, in one or two sentences>
## Complete when
<the explicit completion criteria the worker reports against — this is required>
## Scope & constraints
<files, invariants, pitfalls>
## Report back
<what the worker returns on `done`: a concise result/summary, NOT a raw dump>
```
Worker assignment: if the task ask names a worker, use it; otherwise assign
from the roster (recency-ranked). List the eligible roster in the plan so the
user can reassign, annotating each with what it runs on:
`<nick> (Opus 4.8 / Claude Code @ studio-mbp-01)` (append `@ <host>` when the
peer reported one; omit the parens when the peer advertised no
`model`/`harness`/`host`). Keep each task's brief under ~2,500 characters (the wire
caps a message near 3,000).

Then **call `ExitPlanMode`** to present the plan. The **user approves** (the
user-driven exit), and that approval is the "send these" signal. If the user
keeps planning / edits (reassigns a worker, edits a brief, adds/removes a
task), revise and `ExitPlanMode` again. On approval, continue below.

## Create the tasks

For **each** task, create it on its worker with a directed `SendMessage`. The
**worker mints the task id** and returns the `Task` in the JSON-RPC response —
capture `result.task.id` as that task's `task_id` (you do not choose it):

```bash
agent-gossip a2a call --swarm "$SWARM" --nickname "$NICKNAME" --to "$WORKER" \
  --method SendMessage --text "$BRIEF"
```

The brief itself should ask the worker to **report a result back** (that is
what makes this a report-back task rather than a handover). Handle errors per
create:
- `unknown participant` ⇒ that peer left between the roster read and the create;
  drop that task (note it in its todo) and continue with the rest.
- `message too large` ⇒ shorten that task's brief and retry once.

## Drive each task

The per-task mechanics live in the create/join event handler (loaded for the
session) — do not duplicate them here. (On the CLI fallback rather than Monitor,
the worker's status/artifact legs arrive on the poll tick, not instantly — same
handling, slightly later.) For **each** task's `task_id`:

- **`state:"input-required"` with a question** — the worker needs input; answer
  from your task context with a follow-up message (`agent-gossip a2a call --to $WORKER
  --method SendMessage --task-id "$TASK_ID" --text "<answer>"`). Silent (todo
  only).
- **`task_progress`** — a liveness/percent beat; refresh that task's todo, never
  a printed line.
- **`kind:"artifact-update"` from the worker** ("here is my result") — the
  `body` is that task's **result**. **Print it**, attributed to the worker (it
  is the deliverable), then **approve** it with a follow-up message
  (`--method SendMessage --task-id "$TASK_ID" --text "approved"`). Send a
  change request instead only if the result plainly misses the completion
  criteria and a revision is worth a round trip. The worker then emits
  `state:"completed"`, which closes that task.
- **`state:"failed"` / `task_timeout`** — that task's worker dropped out or
  couldn't finish; record it (no result) and move on. Other tasks are
  unaffected.

Tasks are independent — there is no waiting for all of them, and no step that
runs once they all finish.

## Track tasks in the to-do list

Use your harness's native to-do list as the **single source of truth** for task
status — never a printed status block. It's **`TodoWrite`** in most harnesses;
where that tool is absent, use **`TaskCreate`** (`subject` = the `content` line
below, `activeForm` = `activeForm`) + **`TaskUpdate`** (status
`pending → in_progress → completed`, `deleted` to drop), one task per
`task_id`. The lifecycle is identical either way; wherever this skill says
`TodoWrite` or "todo", use whichever tool your harness provides. On send, add
**one todo per task**:

- `content` is **exactly** `💬 <one-line task> · <worker>` (e.g. `💬 review
  src/net · <crystal-azure>`), status `in_progress`. The companion
  **`activeForm`** uses the same text without the `💬`, e.g.
  `activeForm: "review src/net · <crystal-azure>"`. Write the nickname as
  `<worker>` with literal angle brackets and **no backticks** in **both**
  fields — the widget shows text verbatim: markdown isn't rendered (backticks
  would show literally) and `<`/`>` aren't escaped. Use this exact format;
  don't invent a `task to <worker>` phrasing.
- Move each todo through its lifecycle off that task's events
  (`working`/`input-required`/`task_progress`) via `TodoWrite`; when the worker
  emits `completed` (after your approval) set it `completed`. On
  `failed`/`task_timeout` set it `completed` and note "dropped (failed/timed
  out)" **in the todo content**.

Re-invoking `/gossip:task` appends new todos to this same list.

## Output

The only things this skill prints are: the not-in-swarm guard line, plan mode,
the to-do list, and **each worker's returned result** when its task finishes
(attributed to the worker). There are **no** per-leg status lines, no `💬 tasks`
block, and no narration of transitions — the to-do list carries all in-flight
status. When there is nothing left to drive, **end silently**.
