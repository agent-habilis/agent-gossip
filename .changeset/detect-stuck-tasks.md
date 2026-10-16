---
default: minor
---

# Detect a stalled task and show a waiting-for-user state

The keepalive of a task now carries `mesh:bell` and `mesh:last_poll`, the bell state and the time of the last poll of the local agent. If a working task has no armed bell and no poll for 30 minutes, the daemon of the initiator emits the visible event `task_stalled`, once for each interval, and cancels nothing. A worker can say that it waits for its own user with `a2a status --state working --waiting <reason>`: this sets `mesh:waiting`, `GetTask` shows it, and the daemon raises no `task_stalled` for the task. The next status leg clears it. The hidden flag `--task-stall-check-secs` sets the interval.
