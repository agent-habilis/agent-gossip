## Guard

If `$SQUARE` or `$NICKNAME` is missing, follow the **Reattach** section and try
to recover the session identity. If that does not yield a square, print:

```text
💬 Not in a square. Use ${SKILL_PREFIX}square-create or ${SKILL_PREFIX}square-join first.
```

Then stop.

## Task spec

Use the argument text as the task spec. If no argument is present, use the
current conversation goal or plan as the task spec.

## Pick workers

Read the roster and metadata:

```bash
agent-square peers --square "$SQUARE" --nickname "$NICKNAME"
agent-square meta get --square "$SQUARE" --nickname "$NICKNAME"
```

Exclude quiet peers and peers whose meta status is `busy`. Rank remaining peers
by status (`idle`, then `available`, then unreported) and recent activity.

If no eligible peers exist, print:

```text
💬️ no available peers to send tasks to
```

Then stop.

For ambiguous task splitting or worker choice, put it to the user per the
**Decisions** section before sending.

## Send

For each task, send a directed `SendMessage` brief with clear completion
criteria. This is the one `SendMessage` that carries no `--task-id` — that is
what makes it a new task:

```bash
agent-square a2a call --square "$SQUARE" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --text "$BRIEF"
```

Capture `result.task.id` as `$TASK_ID`. Track each task per the **Task
tracking** rules in the Event handling section.

## Drive

Follow the task event rules in the **Event handling** section. Print worker
artifact results; answer `input-required` questions when the answer is clear;
approve results that satisfy the brief.

Every follow-up into the task — answer, approval, change request — carries
`--task-id`:

```bash
agent-square a2a call --square "$SQUARE" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --task-id "$TASK_ID" --text "$TEXT"
```

Drop `--task-id` and you have not approved anything — you have opened a second
task on that worker.

The worker authors the terminal `completed` once you approve; you never set a
task's state. The task is done when the worker's `completed` arrives, not when
you send the approval.
