---
name: handover
description: Hand a task to another peer in the mesh. Use when the user wants to delegate work to another agent. Task-first - $ARGUMENTS is the task to delegate (else the current plan); composes a brief, then picks a worker, then creates the task and hands it off once the worker accepts.
---

## What this does

Hands a task to another participant. A handover is one delegation **flow** of
the mesh's **A2A task**: a directed `SendMessage` creates it (the worker mints
the task id). The flow is **task-first**: establish the task, build a **plan in
plan mode** (that plan *is* the brief you send), *then* pick the worker, then
create the task. The handover completes at the **handoff** — the moment the
worker **accepts** (`state:"working"`) — not at the worker's execution: once it
accepts you are finished, and it then runs the work on its own. Every leg is
surfaced only to the two parties.

## Silent execution

Run the whole skill **silently**. Do NOT narrate steps, echo variables
(e.g. `$TASK_ID = …`), print commands or their output, or announce what you
are about to do. The roster read and the `task_id` stay in context, unprinted.
The **only** things that ever appear are: the not-in-mesh guard line (when
it applies), **plan mode** (the drafted plan), the **worker picker**
`AskUserQuestion`, and the native **`TodoWrite`** to-do list. There are **no**
printed status or outcome lines — all task status lives in the to-do list.

**Absolute rule (not a blocklist):** a silent step emits its tool call and
**zero** surrounding prose — no preamble *before* it and no postamble *after*
it. This is two-directional: never **announce an upcoming action** ("Now I'll
track this in the to-do list", "Let me add this to the to-do list", "I'll
update the to-do list") and never **report a completed transition** (a
parenthetical aside, or a line narrating having stayed silent, e.g.
`(handover confirmed and closed silently — todo marked completed)`). The
`TodoWrite` call simply happens. Do not invent your own variant of either: if a
sentence announces or reports what the task is/was doing, it does not belong on
screen, period.

## Pre-flight: guard

If you hold `$MESH`/`$NICKNAME` from a `/mesh:create` or `/mesh:join`
`ready` event this session, proceed. Otherwise try to reattach first:
follow `../shared/reattach.md` (resolved relative to this SKILL.md's
directory). Only if reattach also yields no mesh, print:
```
💬 Not in a mesh. Use /mesh:create or /mesh:join first.
```
and STOP.

## The task

Establish *what* is being handed over **before** choosing who does it:

- **If `$ARGUMENTS` is non-empty**, it **is** the task to delegate (e.g.
  `/mesh:handover review folder src/` ⇒ task = "review folder src/").
- **Otherwise**, the task is your current conversation/plan.

## Enter plan mode and build the plan

Go **straight into plan mode**: as the first action after establishing the
task, **call the `EnterPlanMode` tool** — this enters plan mode and shows the
plan-mode UI. Do this *before* anything else (before drafting).

Then, **inside plan mode** and silently:

1. Draft the plan for the task. The plan you write **is** the brief you hand
   over. Keep it under ~2,500 characters (the wire caps a message near
   3,000); push extra detail into the later Q&A. Structure it so the receiver
   can act on it:
   ```
   ## Task
   <what is being done, in one or two sentences>
   ## Goal (complete when)
   <the completion criteria>
   ## Current state
   <what is already done / where things stand>
   ## Next steps
   <the immediate next actions>
   ## Constraints & gotchas
   <anything that will bite the receiver: invariants, pitfalls, files>
   ```

Then **call `ExitPlanMode`** to present the plan — this surfaces the native
"Approve / Keep planning" UI. The **user approves** (the user-driven exit),
and that approval is the "send this" signal. Do **not** show an
`AskUserQuestion` for the brief. If the user keeps planning / edits, revise
and call `ExitPlanMode` again. On approval, the approved plan is `$BRIEF`;
continue below.

## Pick the worker

Now that the task is set, choose who runs it. The roster (`agent-mesh peers`) carries
connectivity; what each peer **runs on** lives in the **meta** channel (peers
self-report it there, the binary does not). Read both, silently — don't print
either:

```bash
agent-mesh peers --mesh "$MESH" --nickname "$NICKNAME"
agent-mesh meta get --mesh "$MESH" --nickname "$NICKNAME"
```

`peers` returns
`{"ok":true,"participants":[{"nickname","last_seen_secs_ago","quiet","reach"}…],"participant_count":N}`.
`meta get` returns `{"ok":true,"document":{"peers":{"<nick>":{"model","harness","host","status"}…}},…}` —
look up each peer's `model`/`harness`/`host`/`status` by nickname in
`document.peers` (absent ⇒ that peer has not reported yet). Drop any entry with
`"quiet":true`, **and any whose `document.peers[<nick>].status` is `"busy"`**
(that peer is not accepting work; `idle`/`available`/absent stay eligible). Rank
the rest by availability then recency: `idle` ahead of `available` ahead of
unreported, and within each by `last_seen_secs_ago` ascending (most recently
active first).
Show an `AskUserQuestion` — question "Hand `<one-line task>` to which peer?",
header `mesh:handover`, options = the **top 3** by recency. For each option:
- **label** = the nickname wrapped in angle brackets, e.g. `<cable-spark>`
  (not `cable-spark`).
- **description** = the peer's `model` / `harness`, the `host` after `@`, then
  recency, e.g. `Opus 4.8 / Claude Code @ studio-mbp-01 · active 3s ago`. The
  widget renders the description as dimmed secondary text. Omit the metadata
  part when the peer advertised none (just `active Ns ago`); join
  `model`/`harness` with ` / `, append `@ <host>` when present, and show just
  whichever parts the peer advertised.

The free-text "Other" entry lets the user type a nickname; re-validate it
against the roster. The chosen nickname (without the brackets) is `$TARGET`.

If the roster has no eligible peers, print `💬️ no available peers to hand over to`
and STOP.

## Create the task

The plan (`$BRIEF`) was already approved in plan mode and the worker picked,
so create straight away. The brief should **hand the work over** (ask the
worker to take it and run it, not to report a result). The **worker mints the
task id** and returns the `Task` — capture `result.task.id` as `$TASK_ID`:

```bash
agent-mesh a2a call --mesh "$MESH" --nickname "$NICKNAME" --to "$TARGET" \
  --method SendMessage --text "$BRIEF"
```

Handle errors from the command:

- `unknown participant` ⇒ the peer left between the roster read and the
  create; print the error and STOP.
- `message too large` ⇒ shorten the brief and retry once.

Open the tasks widget (see below) with this task in progress.

## The handoff completes when the worker accepts

A handover is done the moment the worker takes it — you never wait for the
work. The full sender handling lives in the create/join event handler (loaded
for the session). In short, for this `task_id`:

- **`state:"working"` from the worker** (it accepted) — the handoff is
  complete: set the todo `completed` and **stop watching**. The worker runs the
  work on its own; there is nothing for you to review, approve, or confirm.
- **`state:"input-required"` with a question** — answer from your task context
  with a follow-up message (`agent-mesh a2a call --to $TARGET --method SendMessage
  --task-id "$TASK_ID" --text "<answer>"`). Silent (widget only).
- **`state:"failed"` / `task_timeout`** — the worker passed or dropped; record
  the reason and stop.

You never wait for the worker to *run* the work.

## Track the task in the to-do list

Use your harness's native to-do list as the **single source of truth** for
handover status — **not** a printed `💬 tasks` block. It's **`TodoWrite`** in
most harnesses; where that tool is absent, use **`TaskCreate`** (`subject` = the
`content` line below, `activeForm` = `activeForm`) + **`TaskUpdate`** (status
`pending → in_progress → completed`, `deleted` to drop), one task per
`task_id`. The lifecycle is identical either way; wherever this skill says
`TodoWrite` or "todo", use whichever tool your harness provides. Add one
todo for this handover and keep it updated as the daemon emits events for this
`task_id`; never print a per-update status line.

- Add it on send: a todo whose `content` is **exactly** `💬 handover to
  <$TARGET>` (e.g. `💬 handover to <crystal-azure>`), status `in_progress`.
  The `💬` prefix labels it as a mesh task (`TodoWrite` has no widget title).
  The companion **`activeForm`** uses the same text without the `💬`, e.g.
  `activeForm: "handover to <crystal-azure>"`. Write the nickname as
  `<$TARGET>` with literal angle brackets and **no backticks** in **both**
  fields — the widget shows text verbatim: markdown isn't rendered (backticks
  would show literally) and `<`/`>` aren't escaped.
- Move it through the lifecycle off the `task` events for this `task_id` by
  calling `TodoWrite` again. `task_progress` (incl. the daemon's keepalive
  beats) just refreshes the todo — **never** a printed line.
- On the worker's `state:"working"` (accept), set it `completed` (the terminal
  "handed over" state). On a terminal `failed`/`timeout`, set it `completed`
  too and note the reason **in the todo content** (not a printed line).

## Output

There are **no printed status or outcome lines** — not even a final "task
handed over" line. The native to-do list (via `TodoWrite`) is the sole status
surface; its `completed` state is the terminal indication. The only other
things that may appear are the not-in-mesh guard line, plan mode, and the
worker picker. No `💬 tasks` text block, no per-leg lines, no narration.

After marking the todo `completed`, **end silently** — do **not** print a
closing or summary sentence (e.g. "The handover is complete — `<peer>` will
run the work on its own."), and do **not** print a parenthetical aside
reporting the close (e.g. "(handover confirmed and closed silently — todo
marked completed)"). Any sentence that describes what just happened to the
task is forbidden, named example or not. Say nothing.
