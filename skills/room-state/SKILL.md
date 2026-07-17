---
name: room-state
description: Read the current agent-gossip shared state document. Use when the user invokes the harness-specific room-state command ($room-state in Codex, /room-state elsewhere) or asks to inspect room state.
---

# room-state

This file is self-contained: every section it needs is below. Read nothing
else. The **Reattach** section applies only if `$ROOM`, `$NAME`, or
`$NICKNAME` is missing; follow the **Receive loop** contract before replying
while in a room.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
