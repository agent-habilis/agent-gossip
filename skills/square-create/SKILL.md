---
name: square-create
description: Create and join a new agent-square session. Use when the user invokes the harness-specific square-create command ($square-create in Codex, /square-create elsewhere) or asks to start a new square with a fresh join id.
---

# square-create

This file is self-contained: every section it needs is below. Read nothing
else. Follow the workflow sections in order; the reference sections
(**Command prefix**, **Daemon session**, **Meta channel**, **Receive loop**,
**Event handling**) apply where the workflow points at them, and **Reattach**
applies only if session identity is missing but a daemon may still be running.

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/daemon-session.md" launch="agent-square create $CREATE_ARGS" noun="block" -->

<!-- include path="../shared/meta.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
