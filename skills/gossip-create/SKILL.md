---
name: gossip-create
description: Create and join a new gossip.
when_to_use: The user invokes the gossip-create command or asks to start a new gossip with a fresh join id.
---

# gossip-create

This file is self-contained: every section it needs is below. Read nothing
else. Follow the workflow sections in order; the reference sections
(**Command prefix**, **Daemon session**, **Meta channel**, **Receive loop**,
**Decisions**, **Event handling**) apply where the workflow points at them, and
**Reattach** applies only if session identity is missing but a daemon may still
be running.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/daemon-session.md" launch="agent-gossip create $CREATE_ARGS" noun="block" bell_prefix="" -->

<!-- include path="../shared/meta.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
