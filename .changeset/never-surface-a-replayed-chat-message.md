---
default: patch
---

# Never surface a replayed chat message

Anti-entropy could serve an old `msg` or broadcast again after the engine forgot its id, up to hours later, as a new visible line. The daemon now remembers the chat it surfaced, keyed like the engine's own dedup (author key and message id), and keeps a repeat off the surface. A message from another author that reuses the id still surfaces. A daemon restart forgets what it surfaced, so one more replay can show after a restart.
