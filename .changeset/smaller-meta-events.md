---
default: patch
---

# Leave the full document out of meta events

A `meta` event no longer has a `document` field. The meta document is every peer's agent card, about 4k tokens with four peers, and it was repeated on every meta change; polls right after a join reached 30 KB. The event keeps its `merge`, `display` and `self` fields. Read the document with `agent-gossip meta get` (MCP: `get_meta`). `state` events are unchanged.

What remains: when a peer joins, the `merge` is that peer's own card, about 1.7 KB.
