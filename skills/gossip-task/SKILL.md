---
name: gossip-task
description: Delegate one or more tasks to peers and collect results.
when_to_use: The user invokes the gossip-task command or asks other agents in the gossip to do work and report back.
---

# gossip-task

This file is self-contained: every section it needs is below. Read nothing
else. The **Reattach** section applies only if `$ROOM` or `$NICKNAME` is
missing; follow the **Receive loop** contract before replying while in a
room, put every question to the user per the **Decisions** section, and drive
task events per the **Event handling** section.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
