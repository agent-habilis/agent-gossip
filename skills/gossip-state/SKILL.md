---
name: gossip-state
description: Read the current agent-gossip shared state document.
when_to_use: The user invokes the gossip-state command or asks to inspect gossip state.
---

# gossip-state

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
