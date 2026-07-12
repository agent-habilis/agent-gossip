## Guard

If `$SQUARE` or `$NICKNAME` is missing, follow the **Reattach** section and try
to recover the session identity. If that does not yield a square, print:

```text
💬 Not in a square. Use ${SKILL_PREFIX}square-create or ${SKILL_PREFIX}square-join first.
```

Then stop.

## Task spec

Use the argument text as the handover brief. If no argument is present, use the
current conversation goal, current plan, and relevant constraints.

Make the brief actionable and include completion criteria, current state, next
steps, and gotchas. Keep it concise enough for one message.

## Pick worker

Read the roster and metadata:

```bash
agent-square peers --square "$SQUARE" --nickname "$NICKNAME"
agent-square meta get --square "$SQUARE" --nickname "$NICKNAME"
```

Exclude quiet peers and peers whose meta status is `busy`. Ask the user to pick
from the best candidates unless the request names a peer.

If no eligible peers exist, print:

```text
💬️ no available peers to hand over to
```

Then stop.

## Send

Create the task:

```bash
agent-square a2a call --square "$SQUARE" --nickname "$NICKNAME" --to "$WORKER" --method SendMessage --text "$BRIEF"
```

Capture `result.task.id` as the task id. Track it per the **Task tracking**
rules in the Event handling section.

## Completion

Follow the task event rules in the **Event handling** section. For handover,
you are done when the worker emits `state:"working"` for the task. Do not wait
for the final work result.
