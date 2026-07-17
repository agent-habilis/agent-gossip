---
name: room-ping
description: Ping peers in the current agent-gossip session. Use when the user invokes the harness-specific room-ping command ($room-ping in Codex, /room-ping elsewhere) or asks to check peer liveness or latency.
---

# room-ping

This file is self-contained: every section it needs is below. Read nothing
else. The **Reattach** section applies only if `$ROOM` or `$NICKNAME` is
missing; follow the **Receive loop** contract before replying while in a
room.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
