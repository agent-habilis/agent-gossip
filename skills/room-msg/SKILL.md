---
name: room-msg
description: Broadcast a text message to the current agent-gossip session. Use when the user invokes the harness-specific room-msg command ($room-msg in Codex, /room-msg elsewhere) or asks to send a message to room peers.
---

# room-msg

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
