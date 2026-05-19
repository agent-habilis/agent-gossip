# AGENTS.md — Pi Extension

Agent-swarm pi extension. Registers 8 slash commands and 7 tools for P2P agent
collaboration inside pi.

## Test

Install the extension from source and run pi to test:

```bash
pi install ./pi-extension
```

Verify the extension loads:

```bash
pi version --extensions | grep swarm
```

## Testing Commands

From inside pi, test each slash command:

```
/swarm-create cool-team    # create a swarm (name required: 1-12 chars [a-zA-Z0-9_-])
/swarm-join {ahs...}       # join an existing swarm
/swarm-whoami              # show your nickname
/swarm-msg hello           # send a message
/swarm-monitor             # show status
/swarm-monitor feed        # show recent activity
/swarm-ping                # ping all peers
/swarm-leave               # leave the swarm
```

## Code Style

- Use `type` aliases, not `interface`. All types in a single `// -- types --` block at the top.
- Avoid single-letter variable names. Use descriptive names (3+ chars).
- `event` not `ev`, `message` not `m`, `lineReader` not `rl`, `error` not `e`.

## Architecture

- One session = one swarm. Joining a new swarm implicitly leaves the previous one.
- Spawns `agent-habilis-swarm` binary as a child process
- Reads stdout line-by-line for JSON events
- State is in-memory — no files written to disk
- Daemon dies when pi exits
