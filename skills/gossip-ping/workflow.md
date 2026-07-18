## Guard

If `$GOSSIP` or `$NICKNAME` is missing, follow the **Reattach** section and try
to recover the session identity. If that does not yield a gossip, print:

```text
💬 Not in a gossip. Use ${SKILL_PREFIX}gossip-create or ${SKILL_PREFIX}gossip-join first.
```

Then stop.

## Trigger

Run:

```bash
agent-gossip ping --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Print nothing else. The daemon emits a later `ping_report` event; render it per
the **Event handling** section.
