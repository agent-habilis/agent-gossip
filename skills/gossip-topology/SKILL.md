---
name: gossip-topology
description: Draw the mesh topology of the current gossip as text.
when_to_use: The user invokes the gossip-topology command or asks how the peers connect, which path each peer uses, or who is reached through other peers.
---

# gossip-topology

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
