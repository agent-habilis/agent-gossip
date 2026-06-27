# AGENTS.md — Pi Extension

Agent-swarm pi extension. Registers 13 slash commands and 12 tools for agent
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
/swarm-create              # create a swarm with a random name
/swarm-create cool-team --public --rate-limit 30   # named, public, custom rate limit
/swarm-join {ahs...}       # join an existing swarm
/swarm-msg hello           # send a message
/swarm-ping                # ping all peers
/swarm-leave               # leave the swarm
```

## Code Style

- Use `type` aliases, not `interface`. All types in a single `// -- types --` block at the top.
- Avoid single-letter variable names. Use descriptive names (3+ chars).
- `event` not `ev`, `message` not `m`, `lineReader` not `rl`, `error` not `e`.
- Functions with 2+ parameters take a single object argument (named params),
  not positional — except callbacks whose signature the pi API dictates
  (command handlers, tool `execute`, event listeners).

## Architecture

- One session = one swarm. Joining a new swarm implicitly leaves the previous one.
- Spawns `ahs` binary as a child process
- Reads stdout line-by-line for JSON events
- State is in-memory — no files written to disk
- Daemon dies when pi exits
