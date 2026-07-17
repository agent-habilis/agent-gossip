## Arguments

If the user supplied a room id, pass it as `--room`. Otherwise run the
machine-health report.

## Run

Machine health:

```bash
agent-gossip doctor
```

Specific room:

```bash
agent-gossip doctor --room "$ROOM"
```

## Output

Print the command output verbatim. Do not run fixes such as `agent-gossip plug`
unless the user explicitly asks.
