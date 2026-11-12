---
default: major
---

# Follow the engine: signed slices for each author, no anti-entropy; frame version is now 18.0

The engine now keeps one signed slice for each author in the shared state and in the meta channel. It keeps no message log, no hash chain and no DAG. A peer on frame version 14.0 cannot read frames of version 18.0. All peers of a mesh must update together.

For a user:
- `state get` and `meta get` now print one entry for each member, keyed by the author key in lowercase hex: `{"<author hex>":{...}}`. They do not print one merged document. A write changes your own entry only. The author key changes on each start of the daemon, so the entry of a member moves to a new hex key when it restarts.
- The Agent Card of a member is no longer in the meta channel. `meta get` has no `card`, `card_error` or endpoint hint. The local daemon builds the card of a member from its roster, with the description, skills, modes and version of this daemon, and serves it at `/peers/<peer>/.well-known/agent-card.json`. The relaying interface names the hex key of the member.
- The `fork` event is gone. The engine cannot see equivocation now.
- The poll record of a message has no `seq`, `prev` or `parents` field.
- The flag `--antientropy-interval-secs` is now `--backfill-interval-secs`. The flag `--antientropy-max-resend` is gone.
- A refused state write has a new reason. The reasons `not_synced`, `state_full`, `too_deep` and `clock_ahead` are gone. The new reasons are `too_large`, `rate_limited`, `too_many_authors`, `gone_key`, `malformed`, `wrong_channel`, `draft_mismatch`, `stale_draft` and `seal`. The reasons `malformed` and `seal` mean an engine failure. A merge whose top level is not an object has no reason.
- A lost shard of a split body heals only through the `shard/repair` request, which the sender serves from its cache. A node serves at most 8 such asks from one member and 32 in all in each 5 s window. It drops an ask over the budget with no reply, and the requester asks again on its next tick.
