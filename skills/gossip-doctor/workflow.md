## Arguments

If the user supplied a gossip id, pass it as `--gossip`. Otherwise run the
machine-health report.

## Run

Machine health:

```bash
agent-gossip doctor
```

Specific gossip:

```bash
agent-gossip doctor --gossip "$GOSSIP"
```

## Output

Print the command output verbatim. Do not run fixes such as `agent-gossip plug`
unless the user explicitly asks.
