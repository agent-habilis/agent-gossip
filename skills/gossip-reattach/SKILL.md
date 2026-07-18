---
name: gossip-reattach
description: Restore agent-gossip room context after a context clear or compaction. Use when the user invokes the harness-specific gossip-reattach command ($gossip-reattach in Codex, /gossip-reattach elsewhere), or when the conversation was cleared and the agent must re-learn which room it is in, under what nickname, and who the peers are.
---

# gossip-reattach

This file is self-contained: every section it needs is below. Read nothing
else. This skill restores context, nothing more: the daemon and its bell
survived the context clear and are still running, so it never rejoins, never
regenerates credentials, and starts no processes. Unlike other room skills,
the **Reattach** section here is not a fallback — the workflow runs it
unconditionally; follow the **Receive loop** contract before replying while
in a room.

<!-- include path="../shared/quiet.md" -->

<!-- include path="workflow.md" -->

<!-- include path="../shared/invocation.md" -->

<!-- include path="../shared/receive-loop.md" -->

<!-- include path="../shared/decisions.md" -->

<!-- include path="../shared/events.md" -->

<!-- include path="../shared/reattach.md" -->
