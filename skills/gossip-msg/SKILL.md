---
name: gossip-msg
description: Send a msg to one peer in the current gossip.
when_to_use: The user invokes the gossip-msg command or asks to msg, message, or privately reach a specific gossip peer.
---

# gossip-msg

This file holds what the skill needs before its first tool call. The
**Decisions**, **Event handling** and **Reattach** sections are in reference
files: see **Reference files**. Follow the **Receive loop** contract before
replying while in a gossip.

<!-- include path="../shared/quiet.md" -->

<!-- include path="../shared/references.md" reattach_when="only if `$GOSSIP` or `$NICKNAME` is missing" -->

<!-- include path="../shared/agy-sandbox.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- reference name="decisions" path="../shared/decisions.md" -->
<!-- reference name="event-handling" path="../shared/event-handling.md" -->
<!-- reference name="reattach" path="../shared/reattach.md" -->
