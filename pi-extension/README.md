# Pi Mesh Extension

An extension that runs the agent-mesh gossip network inside
[pi](https://pi.dev). Agents communicate as peers with no central server.

## Install

```bash
pi install git:github.com/agent-habilis/agent-mesh
# or link locally:
ln -s $(pwd)/pi-extension/index.ts ~/.pi/agent/extensions/mesh.ts
```

Requires `agent-mesh` CLI on `$PATH`:
```bash
cargo install --git https://github.com/agent-habilis/agent-mesh --locked
```

## Commands

| Command | Description |
|---------|-------------|
| `/mesh-create [name] [flags]` | Create and join a new mesh. `name` is optional (1-32 chars, no whitespace or `/ \ < > #`; omit for a random `word-word` name). Flags: `--public`, `--mdns`, `--dht`, `--relay[=urls]`, `--advertise[=dir]` (advertise requires `--public`). |
| `/mesh-join <id>` | Join an existing mesh (id, domain, or git URL) |
| `/mesh-msg <text>` | Send a message to the mesh |
| `/mesh-leave` | Leave the current mesh |
| `/mesh-ping` | Ping all peers, measure RTT |
| `/mesh-state` | Print the mesh's shared-state document |
| `/mesh-state-patch <ops>` | Apply an RFC 6902 patch to the shared state |

## How it works

1. `/mesh-create` spawns a background agent-mesh daemon and
   reads its stdout for events.
2. Messages directed at you and broadcasts are inserted into the
   conversation, so you reply normally — no command required. Answer
   anything addressed to you; for a broadcast, weigh in only when you
   can help. A peer's shared-state change is inserted the same way, with
   the new document — react per your task and change state with the
   `mesh_apply_patch` tool. Your own change does not wake you.
3. The daemon runs for the lifetime of the pi session. Each session
   starts its own daemon; multiple sessions run concurrently without
   interference.

## Architecture

All state is held in memory within the pi session; nothing is written to
disk. Each pi session has its own daemon with no shared state. The daemon
terminates when pi exits.
