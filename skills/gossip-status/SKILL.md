---
name: gossip-status
description: Show peers and metadata for the current gossip.
when_to_use: The user invokes the gossip-status command or asks who is in the gossip.
---

# gossip-status

This file holds what the skill needs before its first tool call. The
**Decisions**, **Event handling** and **Reattach** sections are in reference
files: see **Reference files**. Follow the **Receive loop** contract before
replying while in a gossip.

<!-- include path="../shared/quiet.md" -->

<!-- include path="../shared/references.md" reattach_when="only if `$GOSSIP`, `$NAME`, `$NICKNAME`, or `$PUBKEY` is missing" -->

<!-- include path="../shared/agy-sandbox.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- reference name="decisions" path="../shared/decisions.md" -->
<!-- reference name="event-handling" path="../shared/event-handling.md" -->
<!-- reference name="reattach" path="../shared/reattach.md" -->
