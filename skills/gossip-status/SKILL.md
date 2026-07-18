---
name: gossip-status
description: Show peers and metadata for the current gossip.
when_to_use: The user invokes the gossip-status command or asks who is in the gossip.
---

# gossip-status

This file is self-contained: every section it needs is below. Read nothing
else. The **Reattach** section applies only if `$GOSSIP`, `$NAME`, or
`$NICKNAME` is missing; follow the **Receive loop** contract before replying
while in a gossip.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
