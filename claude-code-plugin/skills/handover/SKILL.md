---
name: handover
description: Hand a task to another peer in the swarm. Use when the user wants to delegate work to another agent. Task-first - $ARGUMENTS is the task to delegate (else the current plan); composes a brief, then picks a worker, then drives the exchange until the receiver accepts.
---

## What this does

Hands a task to another participant. A handover is one **behavior** built
on the swarm's generic **task** mechanism: a directed, phased exchange
correlated by a `task_id`. The flow is **task-first**: establish the task,
build a **plan in plan mode** (that plan *is* the brief you send), *then*
pick the worker, then drive the exchange. The handover completes at the
**handoff** — `offer → accept → [context] → done → confirm` — not at the
receiver's execution: the receiver requests close (`done`), you confirm,
and you are finished; the receiver then runs the work on its own. Every leg
is surfaced only to the two parties.

## Silent execution

Run the whole skill **silently**. Do NOT narrate steps, echo variables
(e.g. `$TASK_ID = …`), print commands or their output, or announce what you
are about to do. The roster read and the `task_id` stay in context, unprinted.
The **only** things that ever appear are: the not-in-swarm guard line (when
it applies), **plan mode** (the drafted plan), the **worker picker**
`AskUserQuestion`, and the native **`TodoWrite`** to-do list. There are **no**
printed status or outcome lines — all task status lives in the to-do list.

**Absolute rule (not a blocklist):** any prose that describes a task
transition is forbidden — a transition emits a `TodoWrite` call and **zero**
characters of prose. This includes parenthetical status asides and any line
that narrates having stayed silent (e.g. `(handover confirmed and closed
silently — todo marked completed)`). Do not invent your own variant of such a
line: if a sentence reports what just happened to the task, it does not belong
on screen, period.

## Pre-flight: guard

If you are not in a swarm this session (no `$SWARM`/`$NICKNAME` from a
`/swarm:create` or `/swarm:join` `ready` event), print:
```
Not in a swarm. Use /swarm:create or /swarm:join first.
```
and STOP.

## The task

Establish *what* is being handed over **before** choosing who does it:

- **If `$ARGUMENTS` is non-empty**, it **is** the task to delegate (e.g.
  `/swarm:handover review folder src/` ⇒ task = "review folder src/").
- **Otherwise**, the task is your current conversation/plan.

## Enter plan mode and build the plan

Go **straight into plan mode**: as the first action after establishing the
task, **call the `EnterPlanMode` tool** — this enters plan mode and shows the
plan-mode UI. Do this *before* anything else (before minting the id or
drafting).

Then, **inside plan mode** and silently:

1. Mint one UUID for this whole handover (reused on every leg) — hold it as
   `$TASK_ID`, don't print it:
   ```bash
   TASK_ID=$(uuidgen | tr 'A-Z' 'a-z')
   ```
2. Draft the plan for the task. The plan you write **is** the brief you hand
   over. Keep it under ~2,500 characters (the wire caps a message near
   3,000); push extra detail into the later Q&A. Structure it so the receiver
   can act on it:
   ```
   ## Task
   <what is being done, in one or two sentences>
   ## Goal (done when)
   <the done-criteria>
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

Now that the task is set, choose who runs it. Query the live roster:

```bash
ah-s peers --swarm "$SWARM" --nickname "$NICKNAME"
```

It returns `{"ok":true,"participants":[{"nickname","last_seen_secs_ago","quiet"}…],"count":N}`
(read it silently — don't print the roster). Drop any entry with
`"quiet":true`; rank the rest by `last_seen_secs_ago` ascending (most
recently active first). Show an `AskUserQuestion` — question "Hand
`<one-line task>` to which peer?", header `swarm:handover`, options = the
**top 3** by recency (label = the nickname wrapped in angle brackets, e.g.
`<cable-spark>`, not `cable-spark`; description = "active Ns ago"). The
free-text "Other" entry lets the user type a nickname; re-validate it against
the roster. The chosen nickname (without the brackets) is `$TARGET`.

If the roster has no eligible peers, print `🐝️ no peers to hand over to`
and STOP.

## Send the offer

The plan (`$BRIEF`) was already approved in plan mode and the worker picked,
so send straight away:

```bash
ah-s task --swarm "$SWARM" --nickname "$NICKNAME" --to "$TARGET" \
  --task-id "$TASK_ID" --kind handover --phase offer --text "$BRIEF"
```

Handle errors from the command:

- `unknown participant` ⇒ the peer left between the roster read and the
  send; print the error and STOP.
- `message too large` ⇒ shorten the brief and retry once.

Your own send echoes back as a `task` `"self":true` event. Open the tasks
widget (see below) with this task `offered`.

## Drive the exchange

The receiver drives the lifecycle; you answer and close. The full sender
state machine lives in the create/join Monitor event handler (loaded for the
session) — do not duplicate it here. In short, for this `task_id`:

- **`context` from the receiver** — answer from your task context with
  `--phase context`. Silent (widget only, see below).
- **`done` from the receiver** ("I have what I need, close the handoff") —
  **auto-confirm**: send `--phase confirm`. A handover has nothing for you to
  verify (the receiver runs it on its own), so there is **no review widget
  and no `change`** — that is an `execute`-kind concern. This closes the
  task.
- **`decline`** — the receiver passed; record the reason and stop.

You never wait for the receiver to *execute* the work.

## Track the task in the to-do list

Use Claude Code's native **`TodoWrite`** tool as the **single source of
truth** for handover status — **not** a printed `🐝 tasks` block. Add one
todo for this handover and keep it updated **through `TodoWrite`** as the
daemon emits events for this `task_id`; never print a per-update status line.

- Add it on send: a todo whose `content` is **exactly** `🐝 handover to
  <$TARGET>` (with the literal nickname, e.g. `🐝 handover to
  <crystal-azure>`), status `in_progress`. The todo `content` is **plain
  text shown verbatim** — write the nickname with literal `<`/`>`, **no
  backticks** and **no HTML entities** (`&lt;`). The `🐝` prefix labels it as
  a swarm task (`TodoWrite` has no widget title).
- The companion **`activeForm`** (the present-continuous spinner text) renders
  on a **different surface that HTML-escapes `<`/`>`** (→ `&lt;…&gt;`), so it
  must use the **bare** nickname with **no angle brackets**, e.g.
  `activeForm: "handover to crystal-azure"`. Never put `<`/`>` (or backticks,
  or entities) in `activeForm` or any spinner/status text — angle brackets
  belong only in the todo `content`.
- Move it through the lifecycle off the `task` events (`offered`/`accepted`/
  …) by calling `TodoWrite` again. `task_progress` (incl. the daemon's
  keepalive beats) just refreshes the todo — **never** a printed line.
- On your `confirm`, set it `completed` (the terminal "handed over" state).
  On a terminal `decline`/`timeout`, set it `completed` too and note the
  reason **in the todo content** (not a printed line).

## Output

There are **no printed status or outcome lines** — not even a final "task
handed over" line. The native to-do list (via `TodoWrite`) is the sole status
surface; its `completed` state is the terminal indication. The only other
things that may appear are the not-in-swarm guard line, plan mode, and the
worker picker. No `🐝 tasks` text block, no per-leg lines, no narration.

After marking the todo `completed`, **end silently** — do **not** print a
closing or summary sentence (e.g. "The handover is complete — `<peer>` will
run the work on its own."), and do **not** print a parenthetical aside
reporting the close (e.g. "(handover confirmed and closed silently — todo
marked completed)"). Any sentence that describes what just happened to the
task is forbidden, named example or not. Say nothing.
