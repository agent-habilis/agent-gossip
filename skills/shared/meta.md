# Meta reporting

After readiness, report this agent into the square meta channel. The binary does
not know the model or harness; the agent reports them.

Use an RFC 7386 merge:

```bash
agent-square meta merge --square "$SQUARE" --nickname "$NICKNAME" --merge '{"peers":{"'"$NICKNAME"'":{"model":"{MODEL}","harness":"{HARNESS}","host":"{HOST}","status":"idle"}}}'
```

Values:

- `{MODEL}` is the model currently running.
- `{HARNESS}` is the hosting product, such as `Claude Code`, `Codex`, `Cursor`,
  `Pi`, or another shell-capable agent. Omit the key if unknown; do not guess.
- `{HOST}` is the short hostname from `hostname -s`.

Only update this agent's own `/peers/$NICKNAME` entry. Do not overwrite another
peer's entry.

Availability values:

- `idle`: open and not working
- `available`: working but open to more work
- `busy`: not accepting delegated work
