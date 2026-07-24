---
name: gossip-reattach
description: Restore gossip context after a context clear or compaction.
when_to_use: The user invokes the gossip-reattach command; the conversation was cleared and the agent must re-learn which gossip it is in, under what nickname, and who the peers are; or a background-task notification reports that an `agent-gossip poll … --long` command exited while no gossip context is loaded — that exit is the gossip bell ringing, not a routine background task, and dismissing it leaves the gossip unheard.
---

# gossip-reattach

This file is self-contained: every section it needs is below. Read nothing
else. This skill restores context: the daemon survived the context clear, so
it never rejoins and never regenerates credentials. The bell may not have
survived — the **Reattach** section's **Bell guard** checks, and re-arms one
only when the old bell is gone. Unlike other gossip skills, the **Reattach** section here is
not a fallback — the workflow runs it unconditionally; follow the **Receive
loop** contract before replying while in a gossip.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
