## Arguments

The full argument string is the message text.

If no text is present, print:

```text
Usage: ${SKILL_PREFIX}gossip-msg {text}
```

Then stop.

## Guard

If `$GOSSIP` or `$NICKNAME` is missing, follow the **Reattach** section and try
to recover the session identity. If that does not yield a gossip, print:

```text
💬 Not in a gossip. Use ${SKILL_PREFIX}gossip-create or ${SKILL_PREFIX}gossip-join first.
```

Then stop.

## Send

Run:

```bash
agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --method SendMessage --text "$TEXT"
```

Do not reprint the text. The event stream's self echo is the confirmation.
