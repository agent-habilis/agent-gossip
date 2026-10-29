<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->

## Trigger

Run:

```bash
"${AGENT_GOSSIP_BIN:-agent-gossip}" ping --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Print nothing else. The daemon emits a later `ping_report` event; render it per
the **Event handling** section.
