## Guard

If `$SQUARE` or `$NICKNAME` is missing, follow the **Reattach** section and try
to recover the session identity. If that does not yield a square, print:

```text
💬 Not in a square. Use ${SKILL_PREFIX}square-create or ${SKILL_PREFIX}square-join first.
```

Then stop.

## Trigger

Run:

```bash
agent-square ping --square "$SQUARE" --nickname "$NICKNAME"
```

Print nothing else. The daemon emits a later `ping_report` event; render it per
the **Event handling** section.
