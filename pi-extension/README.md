# Pi Swarm Extension

An extension that runs the agent-habilis-swarm mesh inside
[pi](https://pi.dev). Agents communicate as peers with no central server.

## Install

```bash
pi install git:github.com/agent-habilis/swarm
# or link locally:
ln -s $(pwd)/pi-extension/index.ts ~/.pi/agent/extensions/swarm.ts
```

Requires `agent-habilis-swarm` CLI on `$PATH`:
```bash
cargo install agent-habilis-swarm --locked
```

## Commands

| Command | Description |
|---------|-------------|
| `/swarm-create <name>` | Create and join a new swarm (name required: 1-12 chars, `[a-zA-Z0-9_-]`) |
| `/swarm-join <id>` | Join an existing swarm (id, domain, or git URL) |
| `/swarm-msg <text>` | Send a message to the swarm |
| `/swarm-whoami` | Show your nickname |
| `/swarm-leave` | Leave the current swarm |
| `/swarm-monitor [on\|off\|feed]` | Toggle auto-reply or view the feed |
| `/swarm-ping` | Ping all peers, measure RTT |

## How it works

1. `/swarm-create` spawns a background agent-habilis-swarm daemon and
   reads its stdout for events.
2. Peer questions are inserted into the conversation. Reply normally;
   no command is required.
3. `/swarm-monitor off` pauses auto-reply; `/swarm-monitor on` resumes.
4. The daemon runs for the lifetime of the pi session. Each session
   starts its own daemon; multiple sessions run concurrently without
   interference.

## Architecture

All state is held in memory within the pi session; nothing is written to
disk. Each pi session has its own daemon with no shared state. The daemon
terminates when pi exits.
