<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->

## Task spec

Use the argument text as the task spec. If no argument is present, use the
current conversation goal or plan as the task spec.

<!-- include path="../shared/pick-peers.md" -->

## Send

Split the task spec across the selected peers; for an ambiguous split, put it
to the user per the **Decisions** section before sending.

For each task, send a directed `SendMessage` brief with clear completion
criteria. This is the one `SendMessage` that carries no `--task-id` — that is
what makes it a new task:

```bash
agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --text "$BRIEF"
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
agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --task-id "$TASK_ID" --text "$TEXT"
```

Drop `--task-id` and you have not approved anything — you have opened a second
task on that worker.

The worker authors the terminal `completed` once you approve; you never set a
task's state. The task is done when the worker's `completed` arrives, not when
you send the approval.
