---
name: gossip-msg
description: Broadcast a text message to the current gossip.
when_to_use: The user invokes the gossip-msg command or asks to send a message to gossip peers.
---

# gossip-msg

This file is self-contained: every section it needs is below. Read nothing
else. The **Reattach** section applies only if `$GOSSIP` or `$NICKNAME` is
missing; follow the **Receive loop** contract before replying while in a
gossip.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
