## Guard

If `$ROOM` or `$NICKNAME` is missing, follow the **Reattach** section and try
to recover the session identity. If that does not yield a room, print:

```text
💬 Not in a room. Use ${SKILL_PREFIX}gossip-create or ${SKILL_PREFIX}gossip-join first.
```

Then stop.

## Trigger

Run:

```bash
agent-gossip ping --room "$ROOM" --nickname "$NICKNAME"
```

Print nothing else. The daemon emits a later `ping_report` event; render it per
the **Event handling** section.
