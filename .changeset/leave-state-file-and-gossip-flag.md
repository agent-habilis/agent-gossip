---
default: patch
---

# Fix `leave` for a daemon with a custom `--state-file`, take `leave --gossip`, sweep the files of dead members, and refuse init as a session pid

- `leave` and `session` find a daemon that was started with a `--state-file` outside the runtime base. The daemon reports the path in its `Info` reply, and the socket that carries it is always in the base. Before, `leave` printed `left: []` and the daemon kept running.
- `leave` takes the gossip as `--gossip <id>`, as `peers`, `topology` and `poll` do. The positional form still works. Naming the gossip twice is an error.
- `leave` and `session` sweep what a dead member leaves in the runtime base: its socket and its chunk folder at once, its tracing log after 7 days, and the member folder when the sweep emptied it. A member that does not answer in time keeps every file.
- `leave`, `session` and `bell-check` refuse `--session-pid` 0 and 1, and a default that falls to init, with an error. Before, `leave --session-pid 1` stopped every daemon on the host. A command that runs from an orphaned shell (its parent is init) now fails with this error where it printed an empty list before.
- A daemon started with no `--owner-pid` on `<runtime base>/sessions/<pid>.json` keeps its member across a restart only when that pid is an ancestor of the daemon: the member key is bound to that process (pid and start time), so a later agent with a reused pid gets a new member. When the pid is dead or is no ancestor, the start makes a new member, removes the old key of that path, and prints one warning on stderr. A state file in any other place is reused as before, and `--owner-pid` wins.
- `peers --help` says that `peer_count` is the active peers plus self. The `--lookup` help and the manual say what the DHT publishes: only the beacon publishes to it, the record of a gossip with a relay lookup holds the relay URL and no IP address, and a DHT-only gossip puts the IP addresses of the beacon in the public DHT. The manual also says that a broadcast is not backfilled. The manual says why the lock files stay beside the state file and how long a stop takes.
- `peers`, `ready`, `session`, the state file and `meta get` show the stable member key of a node as `member` (`null` where no proof of it was verified), beside the `pubkey` of one run. `peers` also shows `own_member`, and `meta get` shows `members`, the member key of each slot of the document. The manual says that a restart keeps the member key and changes the `pubkey`.
