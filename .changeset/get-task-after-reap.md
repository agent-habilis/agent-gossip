---
default: patch
---

# GetTask answers for a finished task after the sweep

`GetTask` and `SubscribeToTask` from the task's own peer now return the last terminal state after the sweep reaps the task, 120 s after its last activity. The answer is a small Task with the state and `mesh:peer` only. Any other caller still gets `task not found`, and `CancelTask` on a reaped task does not change.
