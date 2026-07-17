---
name: gossip-status
description: Show peers and metadata for the current agent-gossip session. Use when the user invokes the harness-specific gossip-status command ($gossip-status in Codex, /gossip-status elsewhere) or asks who is in the room.
---

# gossip-status

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
