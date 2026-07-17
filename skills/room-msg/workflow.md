## Arguments

The full argument string is the message text.

If no text is present, print:

```text
Usage: ${SKILL_PREFIX}room-msg {text}
```

Then stop.

## Guard

If `$ROOM` or `$NICKNAME` is missing, follow the **Reattach** section and try
to recover the session identity. If that does not yield a room, print:

```text
💬 Not in a room. Use ${SKILL_PREFIX}room-create or ${SKILL_PREFIX}room-join first.
```

Then stop.

## Send

Run:

```bash
agent-gossip a2a call --room "$ROOM" --nickname "$NICKNAME" --method SendMessage --text "$TEXT"
```

Do not reprint the text. The event stream's self echo is the confirmation.
