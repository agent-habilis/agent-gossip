---
default: major
---

# Topic gossips use relay transport

A topic gossip now lets payload fall back to the relay when a direct path
fails. The transport is part of the topic id, so a peer on an older version
lands in a different gossip for the same string.
