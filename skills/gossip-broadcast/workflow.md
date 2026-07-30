## Arguments

The full argument string is the message text.

If no text is present, print:

```text
💬 usage: ${SKILL_PREFIX}gossip-broadcast {text}
```

Then stop.

<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->

## Send

Every peer in the gossip sees this. To reach one peer privately, use
`${SKILL_PREFIX}gossip-msg` instead.

Run:

```bash
agent-gossip a2a broadcast --gossip "$GOSSIP" --nickname "$NICKNAME" --text "$TEXT"
```

Do not reprint the text. The event stream's self echo is the confirmation.
