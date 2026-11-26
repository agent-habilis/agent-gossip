---
default: major
---

# Require a check-in from the worker of a task

A `working` status is now a check-in: `a2a status --state working` needs
`--text` (what changed) and `--progress` (0 to 100), and `--waiting` is now
`--waiting-on`. The worker sends one at least every 5 minutes. The initiator
gets a `task_overdue` event after 10 minutes of silence, and the worker gets a
`checkin_due` reminder; `task_stalled` and `--task-stall-check-secs` are gone.
An empty artifact is refused.

Run `agent-gossip plug` after you upgrade: an old skill sends a bare `working`
at accept, and the daemon refuses it.
