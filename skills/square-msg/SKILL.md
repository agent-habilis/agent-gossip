---
name: square-msg
description: Broadcast a text message to the current agent-square session. Use when the user invokes the harness-specific square-msg command ($square-msg in Codex, /square-msg elsewhere) or asks to send a message to square peers.
---

# square-msg

This file is self-contained: every section it needs is below. Read nothing
else. The **Reattach** section applies only if `$SQUARE` or `$NICKNAME` is
missing; follow the **Receive loop** contract before replying while in a
square.

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
