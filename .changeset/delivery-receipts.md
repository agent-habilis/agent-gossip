---
default: minor
---

# Add delivery receipts and reader liveness

A directed msg or task leg now earns a `receipt` event from the recipient: `delivered` when its daemon got the frame, `read` when its agent polled it. The echo of a directed msg or task leg prints only after the send succeeds. `peers` rows show a `reader` key with the bell state and the time of the last poll.
