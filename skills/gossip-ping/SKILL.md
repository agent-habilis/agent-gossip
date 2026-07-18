---
name: gossip-ping
description: Ping peers in the current gossip.
when_to_use: The user invokes the gossip-ping command or asks to check peer liveness or latency.
---

# gossip-ping

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
