---
name: gossip-orchestrate
description: Break a goal into parallel subtasks, delegate them to gossip peers, and verify the results.
when_to_use: The user invokes the gossip-orchestrate command or asks to run work as an orchestra — one orchestrator planning, delegating, and verifying while gossip peers execute subtasks in parallel.
---

# gossip-orchestrate

This file holds what the skill needs before its first tool call. The
**Decisions**, **Event handling** and **Reattach** sections are in reference
files: see **Reference files**. Follow the **Receive loop** contract before
replying while in a gossip, put every question to the user per the **Decisions**
section, and drive task events per the **Event handling** section.

<!-- include path="../shared/quiet.md" -->

<!-- include path="../shared/references.md" reattach_when="only if `$GOSSIP` or `$NICKNAME` is missing" -->

<!-- include path="../shared/agy-sandbox.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- reference name="decisions" path="../shared/decisions.md" -->
<!-- reference name="event-handling" path="../shared/event-handling.md" -->
<!-- reference name="reattach" path="../shared/reattach.md" -->
<!-- reference name="pick-peers" path="../shared/pick-peers.md" -->
