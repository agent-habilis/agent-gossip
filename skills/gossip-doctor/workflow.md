## Arguments

If the user supplied a gossip id, pass it as `--gossip`. Otherwise run the
machine-health report.

## Run

Machine health:

```bash
"${AGENT_GOSSIP_BIN:-agent-gossip}" doctor
```

Specific gossip:

```bash
"${AGENT_GOSSIP_BIN:-agent-gossip}" doctor --gossip "$GOSSIP"
```

## Output

Print the command output verbatim. Do not run fixes such as `agent-gossip plug`
unless the user explicitly asks.
