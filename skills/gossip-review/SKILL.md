---
name: gossip-review
description: Fan out an adversarial review to gossip peers and merge their findings into one report.
when_to_use: The user invokes the gossip-review command or asks peers in the gossip to attack, red-team, or adversarially review a plan, diff, or proposal.
---

# gossip-review

This file is self-contained: every section it needs is below. Read nothing
else. The **Reattach** section applies only if `$GOSSIP` or `$NICKNAME` is
missing; follow the **Receive loop** contract before replying while in a
gossip, put every question to the user per the **Decisions** section, and drive
task events per the **Event handling** section.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
