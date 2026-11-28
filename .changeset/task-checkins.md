---
default: major
---

# Require a check-in from the worker of a task

A `working` status is now a check-in: `a2a status --state working` needs
`--text` (what changed) and `--progress` (0 to 100), and `--waiting` is now
`--waiting-on-user`, a flag. The worker sends one at least every 5 minutes. The
initiator gets a `task_overdue` event after 10 minutes of silence, and the worker gets a
`checkin_due` reminder; `task_stalled` and `--task-stall-check-secs` are gone.
An empty artifact is refused.

The initiator's daemon now mints the task id, and each message carries a
`mesh:seq` (1 on the opening). A `SendMessage` opening that times out returns the task
in state submitted with `mesh:unconfirmed` instead of an error.

Every leg of a task now carries `mesh:seq`, each party on its own counter. A leg with no
`mesh:seq`, a replayed leg, and a leg behind a newer one are dropped whole: if a task ends or
waits for review with no result in sight, ask the worker again with a follow-up on the task.

A new event `task_gap` tells the sender that a question of the worker was lost and the task
waits on it. `task_timeout` has a new reason, `worker-lost`: the worker restarted and lost the
task.

Breaking for callers:

- The JSON status event key `waiting` (a string) is now `waiting_user` (a boolean).
- The MCP `task_status` argument `waiting` (a string) is now `waiting_on_user` (a boolean),
  and a `working` status needs `note` and `progress`.
- In the Rust API, `TaskStatusParams` holds one `WorkerLeg` in place of `state`, `note` and
  `waiting`, built with `TaskStatusParams::new(task_id, leg)`.

Run `agent-gossip plug` after you upgrade: an old skill sends a bare `working`
at accept, and the daemon refuses it.
