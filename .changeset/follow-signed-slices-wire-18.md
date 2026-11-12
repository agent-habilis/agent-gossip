---
default: major
---

# Follow the engine: signed slices for each author, no anti-entropy; frame version is now 18.0

The engine now keeps one signed slice for each author in the shared state and in the meta channel. It keeps no message log, no hash chain and no DAG. A peer on frame version 14.0 cannot read frames of version 18.0. All peers of a mesh must update together.

For a user:
- The `fork` event is gone. The engine cannot see equivocation now.
- The poll record of a message has no `seq`, `prev` or `parents` field.
- The flag `--antientropy-interval-secs` is now `--backfill-interval-secs`. The flag `--antientropy-max-resend` is gone.
- A refused state write has a new reason. The reasons `not_synced`, `state_full`, `too_deep` and `clock_ahead` are gone. The new reasons are `too_large`, `rate_limited`, `too_many_authors`, `gone_key`, `malformed`, `wrong_channel`, `draft_mismatch`, `stale_draft` and `seal`.
- A lost shard of a split body heals only through the `shard/repair` request, which the sender serves from its cache.
