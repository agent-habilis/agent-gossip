---
default: patch
---

# Keep the daemon responsive when a task peer is unreachable

Task heartbeats, task timeout cancels, shard repair requests and shard repair replies now start their sends in the background. Before, a send to a peer with a cold connection dialed on the daemon's event loop, so the daemon stopped for up to 3 s per send and every CLI call waited. With a frozen peer, the slowest `peers` call went from 2.9 s to 0.02 s. The engine change is fofoca's new `ops::deliver_in_background`.
