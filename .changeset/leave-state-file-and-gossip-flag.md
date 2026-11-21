---
default: patch
---

# Find a daemon with a custom `--state-file` in `leave`, and take `leave --gossip`

- `leave` and `session` find a daemon that was started with a `--state-file` outside the runtime base. The daemon reports the path in its `Info` reply, and the socket that carries it is always in the base. Before, `leave` printed `left: []` and the daemon kept running.
- `leave` takes the gossip as `--gossip <id>`, as `peers`, `topology` and `poll` do. The positional form still works. Naming the gossip twice is an error.
- `peers --help` says that `peer_count` is the active peers plus self.
