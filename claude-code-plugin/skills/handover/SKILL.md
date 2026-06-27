---
name: handover
description: Hand a task to another peer in the swarm. Use when the user wants to delegate work to another agent. Task-first - $ARGUMENTS is the task to delegate (else the current plan); composes a brief, then picks a worker, then drives the exchange until the receiver accepts.
---

## What this does

Hands a task to another participant. A handover is one **behavior** built
on the swarm's generic **exchange** mechanism: a directed, phased exchange
correlated by a `exchange_id`. The flow is **task-first**: establish the task,
build a **plan in plan mode** (that plan *is* the brief you send), *then*
pick the worker, then drive the exchange. The handover completes at the
**handoff** — `offer → accept → [context] → done → confirm` — not at the
receiver's execution: the receiver requests close (`done`), you confirm,
and you are finished; the receiver then runs the work on its own. Every leg
is surfaced only to the two parties.

## Silent execution

Run the whole skill **silently**. Do NOT narrate steps, echo variables
(e.g. `$EXCHANGE_ID = …`), print commands or their output, or announce what you
are about to do. The roster read and the `exchange_id` stay in context, unprinted.
The **only** things that ever appear are: the not-in-swarm guard line (when
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

If you are not in a swarm this session (no `$SWARM`/`$NICKNAME` from a
`/swarm:create` or `/swarm:join` `ready` event), print:
```
🐝 Not in a swarm. Use /swarm:create or /swarm:join first.
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
   `$EXCHANGE_ID`, don't print it:
   ```bash
   EXCHANGE_ID=$(uuidgen | tr 'A-Z' 'a-z')
   ```
2. Draft the plan for the task. The plan you write **is** the brief you hand
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

Now that the task is set, choose who runs it. Query the live roster:

```bash
ahsw peers --swarm "$SWARM" --nickname "$NICKNAME"
```

It returns
`{"ok":true,"participants":[{"nickname","last_seen_secs_ago","quiet","reach","model","harness"}…],"count":N}`
(read it silently — don't print the roster). Drop any entry with
`"quiet":true`; rank the rest by `last_seen_secs_ago` ascending (most
recently active first). Show an `AskUserQuestion` — question "Hand
`<one-line task>` to which peer?", header `swarm:handover`, options = the
**top 3** by recency. For each option:
- **label** = the nickname wrapped in angle brackets, e.g. `<cable-spark>`
  (not `cable-spark`).
- **description** = the peer's `model` / `harness` then recency, e.g.
  `Opus 4.8 / Claude Code · active 3s ago`. The widget renders the
  description as dimmed secondary text. Omit the metadata part when the peer
  advertised none (just `active Ns ago`); join `model`/`harness` with ` / `,
  or show just the one present.

The free-text "Other" entry lets the user type a nickname; re-validate it
against the roster. The chosen nickname (without the brackets) is `$TARGET`.

If the roster has no eligible peers, print `🐝️ no peers to hand over to`
and STOP.

## Send the offer

The plan (`$BRIEF`) was already approved in plan mode and the worker picked,
so send straight away:

```bash
ahsw exchange --swarm "$SWARM" --nickname "$NICKNAME" --to "$TARGET" \
  --exchange-id "$EXCHANGE_ID" --kind handover --phase offer --text "$BRIEF"
```

Handle errors from the command:

- `unknown participant` ⇒ the peer left between the roster read and the
  send; print the error and STOP.
- `message too large` ⇒ shorten the brief and retry once.

Your own send echoes back as an `exchange` `"self":true` event. Open the tasks
widget (see below) with this task `offered`.

## Drive the exchange

The receiver drives the lifecycle; you answer and close. The full sender
state machine lives in the create/join event handler (loaded for the session) —
do not duplicate it here. (If that session is on the CLI fallback rather than
Monitor, the receiver's legs arrive on the poll tick, not instantly — same
handling, slightly later.) In short, for this `exchange_id`:

- **`context` from the receiver** — answer from your task context with
  `--phase context`. Silent (widget only, see below).
- **`done` from the receiver** ("I have what I need, close the handoff") —
  **auto-confirm**: send `--phase confirm`. A handover has nothing for you to
  verify (the receiver runs it on its own), so there is **no review widget
  and no `change`** — that is a `task`-kind concern. This closes the
  task.
- **`decline`** — the receiver passed; record the reason and stop.

You never wait for the receiver to *run* the work.

## Track the task in the to-do list

Use your harness's native to-do list as the **single source of truth** for
handover status — **not** a printed `🐝 tasks` block. It's **`TodoWrite`** in
most harnesses; where that tool is absent, use **`TaskCreate`** (`subject` = the
`content` line below, `activeForm` = `activeForm`) + **`TaskUpdate`** (status
`pending → in_progress → completed`, `deleted` to drop), one task per
`exchange_id`. The lifecycle is identical either way; wherever this skill says
`TodoWrite` or "todo", use whichever tool your harness provides. Add one
todo for this handover and keep it updated as the daemon emits events for this
`exchange_id`; never print a per-update status line.

- Add it on send: a todo whose `content` is **exactly** `🐝 handover to
  <$TARGET>` (e.g. `🐝 handover to <crystal-azure>`), status `in_progress`.
  The `🐝` prefix labels it as a swarm task (`TodoWrite` has no widget title).
  The companion **`activeForm`** uses the same text without the `🐝`, e.g.
  `activeForm: "handover to <crystal-azure>"`. Write the nickname as
  `<$TARGET>` with literal angle brackets and **no backticks** in **both**
  fields — the widget shows text verbatim: markdown isn't rendered (backticks
  would show literally) and `<`/`>` aren't escaped.
- Move it through the lifecycle off the `exchange` events (`offered`/`accepted`/
  …) by calling `TodoWrite` again. `exchange_progress` (incl. the daemon's
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
