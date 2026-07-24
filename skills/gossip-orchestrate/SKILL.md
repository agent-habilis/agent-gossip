---
name: gossip-orchestrate
description: Break a goal into parallel subtasks, delegate them to gossip peers, and verify the results.
when_to_use: The user invokes the gossip-orchestrate command or asks to run work as an orchestra — one orchestrator planning, delegating, and verifying while gossip peers execute subtasks in parallel.
---

# gossip-orchestrate

This file is self-contained: every section it needs is below. Read nothing
else. The **Reattach** section applies only if `$GOSSIP` or `$NICKNAME` is
missing; follow the **Receive loop** contract before replying while in a
gossip, put every question to the user per the **Decisions** section, and drive
task events per the **Event handling** section.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
