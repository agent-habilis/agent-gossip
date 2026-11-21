---
default: patch
---

# Fix `leave` for a daemon with a custom `--state-file`, take `leave --gossip`, sweep the files of dead members, and refuse init as a session pid

- `leave` and `session` find a daemon that was started with a `--state-file` outside the runtime base. The daemon reports the path in its `Info` reply, and the socket that carries it is always in the base. Before, `leave` printed `left: []` and the daemon kept running.
- `leave` takes the gossip as `--gossip <id>`, as `peers`, `topology` and `poll` do. The positional form still works. Naming the gossip twice is an error.
- `leave` and `session` sweep what a dead member leaves in the runtime base: its socket and its chunk folder at once, its tracing log after 7 days, and the member folder when the sweep emptied it. A member that does not answer in time keeps every file.
- `leave`, `session` and `bell-check` refuse `--session-pid` 0 and 1, and a default that falls to init, with an error. Before, `leave --session-pid 1` stopped every daemon on the host.
- `peers --help` says that `peer_count` is the active peers plus self. The manual says why the lock files stay beside the state file and how long a stop takes.
