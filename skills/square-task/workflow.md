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

For ambiguous task splitting or worker choice, ask the user before sending.

## Send

For each task, send a directed `SendMessage` brief with clear completion
criteria:

```bash
agent-square a2a call --square "$SQUARE" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --text "$BRIEF"
```

Capture `result.task.id` as the task id. Track each task per the **Task
tracking** rules in the Event handling section.

## Drive

Follow the task event rules in the **Event handling** section. Print worker
artifact results; answer `input-required` questions when the answer is clear;
approve results that satisfy the brief.
