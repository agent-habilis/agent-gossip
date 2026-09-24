---
default: patch
---

# Never surface a replayed task leg

Anti-entropy could serve an old task leg again after the engine forgot its id, up to hours later: `completed` twice, or a stale `working` note after the task closed. The daemon now keeps a leg off the surface when its task is closed, when its task was reaped after closing, or when the same leg id was already surfaced. A leg for a task the daemon does not know yet still surfaces.

One side effect: after your daemon cancels a task, a real late leg from the peer for that task (an artifact, `completed`) is not surfaced either. A daemon restart forgets what it surfaced, so one more replay can show after a restart.
