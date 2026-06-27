---
name: task
description: Send one or more tasks to peers and get their results back. Use when the user wants other agents to run work and report back (e.g. review src/net on one peer, review src/daemon on another). Task-first - $ARGUMENTS is the task(s) to send (else the current plan); each task has its own completion criteria and worker, is tracked in the to-do list, and its result prints when the worker finishes. Re-invoke to add more tasks.
---

## What this does

Sends **one or more tasks** to other participants and collects each result.
Each task is one `task`-kind exchange on the swarm's generic **exchange** mechanism: a
directed, phased exchange correlated by a `exchange_id` where the worker **runs the
work and reports its result** on the `done` leg, and you `confirm` it (or
`change` for a revision). That is the difference from `/swarm:handover`, which
hands a task off and walks away with no result.

Every task is **independent**: its own `exchange_id`, its own worker, its own
clear **completion criteria**, its own to-do entry. There is **no** group-level
outcome — when a worker finishes, its result prints and that task closes. If
you later want something done across the results, ask in a normal turn; this
skill does not encode any cross-task step.

You can send several tasks at once, and you can **re-invoke `/swarm:task`** to
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

If you are not in a swarm this session (no `$SWARM`/`$NICKNAME` from a
`/swarm:create` or `/swarm:join` `ready` event), print:
```
🐝 Not in a swarm. Use /swarm:create or /swarm:join first.
```
and STOP.

## The task(s)

Establish *what* is being sent **before** choosing who runs it:

- **If `$ARGUMENTS` is non-empty**, it **is** the task spec. It may describe a
  single task or several distinct ones (e.g. `/swarm:task review src/net on one
  peer, review src/daemon on another` ⇒ two tasks).
- **Otherwise**, the task is your current conversation/plan (one task).

## Read the roster

Query the live roster (silently — don't print it):

```bash
ahsw peers --swarm "$SWARM" --nickname "$NICKNAME"
```

It returns
`{"ok":true,"participants":[{"nickname","last_seen_secs_ago","quiet","reach","model","harness"}…],"count":N}`.
Drop any entry with `"quiet":true`; rank the rest by `last_seen_secs_ago`
ascending (most recently active first). If there are no eligible peers, print
`🐝️ no peers to send tasks to` and STOP.

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
`<nick> (Opus 4.8 / Claude Code)` (omit the parens when the peer advertised no
`model`/`harness`). Keep each task's brief under ~2,500 characters (the wire
caps a message near 3,000).

Then **call `ExitPlanMode`** to present the plan. The **user approves** (the
user-driven exit), and that approval is the "send these" signal. If the user
keeps planning / edits (reassigns a worker, edits a brief, adds/removes a
task), revise and `ExitPlanMode` again. On approval, continue below.

## Send the offers

Mint **one fresh UUID `exchange_id` per task** (never reuse one — each task is
independent). For each task, send its opening offer to its worker
with that task's brief:

```bash
ahsw exchange --swarm "$SWARM" --nickname "$NICKNAME" --to "$WORKER" \
  --exchange-id "$EXCHANGE_ID" --kind task --phase offer --text "$BRIEF"
```

Handle errors per send:
- `unknown participant` ⇒ that peer left between the roster read and the send;
  drop that task (note it in its todo) and continue with the rest.
- `message too large` ⇒ shorten that task's brief and retry once.

Each send echoes back as a `task` `"self":true` event.

## Drive each task

The per-task mechanics live in the create/join event handler (loaded for the
session) — do not duplicate them here. (If that session is on the CLI fallback
rather than Monitor, the worker's legs arrive on the poll tick, not instantly —
same handling, slightly later.) For **each** task's `exchange_id`:

- **`context` from the worker** — answer from your task context with `--phase
  context`. Silent (todo only).
- **`progress` from the worker** — a liveness/percent beat; refresh that
  task's todo, never a printed line.
- **`done` from the worker** ("here is my result") — the `body` is that task's
  **result**. **Print it**, attributed to the worker (it is the deliverable),
  then **`confirm`**: send `--phase confirm`. Use `--phase change` only if the
  result plainly misses the completion criteria and a revision is worth a round trip.
  Confirm closes that task.
- **`decline` / `exchange_timeout`** — that task's worker dropped out; record it
  (no result) and move on. Other tasks are unaffected.

Tasks are independent — there is no waiting for all of them, and no step that
runs once they all finish.

## Track tasks in the to-do list

Use Claude Code's native **`TodoWrite`** tool as the **single source of truth**
for task status — never a printed status block. On send, add **one todo per
task**:

- `content` is **exactly** `🐝 <one-line task> · <worker>` (e.g. `🐝 review
  src/net · <crystal-azure>`), status `in_progress`. The companion
  **`activeForm`** uses the same text without the `🐝`, e.g.
  `activeForm: "review src/net · <crystal-azure>"`. Write the nickname as
  `<worker>` with literal angle brackets and **no backticks** in **both**
  fields — the widget shows text verbatim: markdown isn't rendered (backticks
  would show literally) and `<`/`>` aren't escaped. Use this exact format;
  don't invent a `task to <worker>` phrasing.
- Move each todo through its lifecycle off that task's events
  (`accepted`/`progress`) via `TodoWrite`; on your `confirm` set it
  `completed`. On `decline`/`exchange_timeout` set it `completed` and note "dropped
  (declined/timed out)" **in the todo content**.

Re-invoking `/swarm:task` appends new todos to this same list.

## Output

The only things this skill prints are: the not-in-swarm guard line, plan mode,
the to-do list, and **each worker's returned result** when its task finishes
(attributed to the worker). There are **no** per-leg status lines, no `🐝 tasks`
block, and no narration of transitions — the to-do list carries all in-flight
status. When there is nothing left to drive, **end silently**.
