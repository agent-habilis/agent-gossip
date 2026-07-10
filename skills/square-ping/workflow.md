# square-ping workflow

## Guard

If `$SQUARE` or `$NICKNAME` is missing, read `../shared/reattach.md` and try to
recover the session identity. If that does not yield a square, print:

```text
💬 Not in a square. Use ${SKILL_PREFIX}square-create or ${SKILL_PREFIX}square-join first.
```

Then stop.

## Trigger

Run:

```bash
agent-square ping --square "$SQUARE" --nickname "$NICKNAME"
```

Print nothing else. The daemon emits a later `ping_report` event; render it via
`../shared/events.md`.
