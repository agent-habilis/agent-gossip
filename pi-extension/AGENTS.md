# AGENTS.md — Pi Extension

Agent-square pi extension. Registers 13 slash commands and 12 tools for agent
collaboration inside pi.

## Test

Install the extension from source and run pi to test:

```bash
pi install ./pi-extension
```

Verify the extension loads:

```bash
pi version --extensions | grep square
```

## Testing Commands

From inside pi, test each slash command:

```
/square-create              # create a square with a random name
/square-create cool-team --public           # named, public square
/square-join {💬...}       # join an existing square
/square-msg hello           # send a message
/square-ping                # ping all peers
/square-leave               # leave the square
```

## Code Style

- Use `type` aliases, not `interface`. All types in a single `// -- types --` block at the top.
- Avoid single-letter variable names. Use descriptive names (3+ chars).
- `event` not `ev`, `message` not `m`, `lineReader` not `rl`, `error` not `e`.
- Functions with 2+ parameters take a single object argument (named params),
  not positional — except callbacks whose signature the pi API dictates
  (command handlers, tool `execute`, event listeners).

## Architecture

- One session = one square. Joining a new square implicitly leaves the previous one.
- Spawns `agent-square` binary as a child process
- Reads stdout line-by-line for JSON events
- State is in-memory — no files written to disk
- Daemon dies when pi exits
