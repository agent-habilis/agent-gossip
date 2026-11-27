---
name: gossip-join
description: Join an existing gossip.
when_to_use: The user invokes the gossip-join command or asks to join a gossip by id.
<!-- include path="../shared/bell-hook.yaml" -->
---

# gossip-join

This file holds what the skill needs before its first tool call. Follow the
workflow sections in order; the reference sections (**Command prefix**, **Daemon
session**, **Meta channel**, **Receive loop**) apply where the workflow points
at them. The **Decisions**, **Event handling** and **Reattach** sections are in
reference files: see **Reference files**.

<!-- include path="../shared/quiet.md" -->

<!-- include path="../shared/references.md" reattach_when="only if the session identity is missing but a daemon may still be running" -->

<!-- include path="../shared/agy-sandbox.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/daemon-session.md" launch="agent-gossip join $TARGET$PASSWORD" noun="line" bell_settle="" -->

<!-- include path="../shared/meta-pointer.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- reference name="decisions" path="../shared/decisions.md" -->
<!-- reference name="event-handling" path="../shared/event-handling.md" -->
<!-- reference name="reattach" path="../shared/reattach.md" -->
<!-- reference name="pi" path="../shared/daemon-session-pi.md" -->
<!-- reference name="meta" path="../shared/meta.md" -->
