---
name: gossip-task
description: Delegate one or more tasks to peers and collect results.
when_to_use: The user invokes the gossip-task command or asks other agents in the gossip to do work and report back.
---

# gossip-task

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
