<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->

## Goal

Use the argument text as the goal. If no argument is present, use the current
conversation goal or plan.

Hold a one-line label of the goal as `$GOAL_LABEL`; it names the run in the
report header and the todo subjects.

<!-- include path="../shared/pick-peers.md" -->

The selected peers are the **workers** for this run. You are the orchestrator:
you plan, dispatch, verify, and never take a subtask yourself — an orchestrator
deep in a subtask stops dispatching, and the whole orchestra idles behind it.

## Plan

Break the goal into subtasks built for parallel execution:

- Each subtask is **self-contained**: a worker gets the brief and nothing else,
  so the brief carries all context needed — the goal it serves, the inputs, and
  where the boundaries to sibling subtasks lie.
- Each subtask carries **completion criteria** concrete enough to verify a
  result against without re-doing the work.
- Cut for independence. Two pieces that must land in the same file or decide
  the same question are one subtask, not two; a piece that needs another's
  output **depends on** it and waits in the queue until that dependency
  completes.
- Cut for the orchestra: at least as many dependency-free subtasks as workers
  when the goal allows it — a worker with nothing ready is wasted. More
  subtasks than workers is normal; the surplus queues.

Put the plan to the user per the **Decisions** section as ONE question — the
subtask list with dependencies marked, a blank line between each numbered
subtask, options `Dispatch` and `Revise`. On `Revise`, ask what to change as
its own follow-up question per the **Decisions** section, apply the changes,
and re-ask the plan question. Only a dispatched plan is final.

## Dispatch

Hold the approved subtasks as the **queue**, dependency-free ones first.

Dispatching a subtask to a worker is one directed `SendMessage` carrying no
`--task-id` — that is what makes it a new task:

```bash
agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --text "$BRIEF"
```

`$BRIEF` is this template with `$NICKNAME`, `$GOAL_LABEL`, and the subtask
spliced in:

```text
ORCHESTRATED SUBTASK · from <$NICKNAME> · goal: $GOAL_LABEL

SUBTASK:
$SUBTASK

COMPLETION CRITERIA:
$CRITERIA

Do exactly this subtask. Siblings are running in parallel, so stay inside its boundaries. Return ONE artifact on this task containing the result and how each completion criterion is met. If blocked on something only I can decide, ask on the task. After the artifact, wait for approval, then close the task.
```

Capture `result.task.id` as that worker's `$TASK_ID`. Track each task per the
**Task tracking** rules in the Event handling section, with the one-line task
slot filled as `orch: $SUBTASK_LABEL`.

**Keep every worker busy.** Start by dispatching one ready subtask to every
worker, and from then on hold the invariant: a worker without a live task gets
the next ready subtask from the queue the moment it frees up — inside the same
turn as the terminal event that freed it, never parked until the wave ends.
There are no waves and no barriers after the first dispatch; workers idle only
when the queue has nothing ready for them.

## Drive

Follow the task event rules in the **Event handling** section and the
**Receive loop** contract — one batch per turn, print last, act first.

On `input-required` kind `artifact-update` — a worker's result — **verify
before approving**: check the artifact against the subtask's completion
criteria, not against a general sense of done.

- Every criterion met → approve with a `--task-id` follow-up so the worker
  closes, then print one line:

  ```bash
  agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --task-id "$TASK_ID" --text "verified — close the task"
  ```

  ```text
  💬 `<$WORKER>` · orch: $SUBTASK_LABEL · verified
  ```

- A criterion missed → send a change request on the same task naming exactly
  the criteria that failed and why; the worker resumes on the same task. A
  subtask that fails verification twice goes to the user per the **Decisions**
  section — re-brief, reassign, or drop.

Drop `--task-id` and you have not approved or requested anything — you have
opened a second task on that worker.

On `input-required` kind `status-update` — a worker's question — answer it
when the answer is clear from the goal and the plan; otherwise put it to the
user per the **Decisions** section. Answer promptly either way: a blocked
worker is an idle worker.

A `completed` after approval frees its worker and may unblock queued
dependents — both effects land in the same turn: mark dependents ready, then
dispatch per the Keep-every-worker-busy invariant. A `failed` or
`task_timeout` also frees its worker; its subtask returns to the front of the
queue for the next free worker, and after a second drop goes to the user per
the **Decisions** section. One carve-out: the eviction clock
keeps running while a result is parked on you, so verify in the same turn the
artifact arrives — before anything goes to the user. A task that drops
*after* its artifact arrived is a lost close, not lost work: the result is in
hand. Verified criteria count the subtask done — do not re-queue it; a
criterion that fails now re-queues the subtask as a fresh dispatch, since the
dead task can carry no change request — and the failure still counts: both
escalation counters (fails-verification-twice, second-drop) follow the
*subtask* across task ids and workers, so a fresh dispatch never resets them.
Never send a holding message to reset the clock: a message leg on a parked
task resumes the worker to `working`, and anything that is not the approval
reads as a change request. When the user drops a subtask, every
subtask depending on it drops with it — remove them from the queue; all of
them appear on the report's `dropped:` line.

A `task_timeout` on a task still unacknowledged (no `working` yet — see the
initiator flow in the Event handling section) is a stalled pickup, surfacing
~2 minutes after dispatch. Its re-dispatch is automatic — orchestrate
overrides the initiator flow's ask-the-user recovery; the second-drop rule
above is the escalation. When re-dispatching, prefer a worker other than the
one that stalled. Preference only: the stalling peer stays a full candidate
for every other subtask, with no ranking demotion.

**Don't idle behind a stalled pickup.** When a worker frees up and the queue
has nothing ready for it, probe each sibling task still unacknowledged with
`GetTask` (see the initiator flow). One still `TASK_STATE_SUBMITTED` ~2
minutes after dispatch is stalled — cancel it rather than waiting out the
daemon's eviction:

```bash
agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method CancelTask --task-id "$TASK_ID"
```

Then print

```text
💬 `<$WORKER>` · orch: $SUBTASK_LABEL · no pickup · reassigning
```

and hand the subtask to the freed worker in the same turn. Cancel before
re-dispatch is not optional: one subtask must never run on two workers.

The worker authors the terminal `completed` once you approve; you never set a
task's state.

## Report

Run this only when the queue is empty and every dispatched task has reached a
terminal state (`completed`, `failed`, `task_timeout`). Close the remaining
todos first, then print the report as the final output of the turn — one line
per subtask in plan order, then the goal's outcome assembled from the verified
results:

```text
💬 orchestrate · $GOAL_LABEL · $W workers · $N subtasks · $D dropped

1. $SUBTASK_LABEL · `<nick-a>` · <one-line result>

2. $SUBTASK_LABEL · `<nick-b>` · <one-line result>

dropped: $SUBTASK_LABEL · <why>

**outcome** · <what the verified results add up to against the goal: done, done with gaps (name them), or blocked (name the blocker)>
```

Omit the `dropped:` line when nothing was dropped.
