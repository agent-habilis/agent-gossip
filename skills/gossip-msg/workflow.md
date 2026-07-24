## Arguments

The full argument string is the message text.

If no text is present, print:

```text
💬 usage: ${SKILL_PREFIX}gossip-msg {text}
```

Then stop.

<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->

## Send

Run:

```bash
agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --method SendMessage --text "$TEXT"
```

Do not reprint the text. The event stream's self echo is the confirmation.
