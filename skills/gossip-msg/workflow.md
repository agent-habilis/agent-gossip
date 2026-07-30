## Arguments

The full argument string is the message text. The recipient is not an
argument — the next section asks for it.

If no text is present, print:

```text
💬 usage: ${SKILL_PREFIX}gossip-msg {text}
```

Then stop.

<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->

<!-- include path="../shared/pick-peers.md" -->

## Send

Send the same text to each of the **selected peers**, one command per peer:

```bash
agent-gossip a2a msg --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$PEER" --text "$TEXT"
```

Each of these is a separate private message: only you and that one peer can
see it. Selecting several peers sends several msgs — it does **not** open a
group conversation, and the recipients cannot see each other's copy or each
other's replies. If the user wants one conversation everybody shares, that is
`${SKILL_PREFIX}gossip-broadcast`.

Do not reprint the text. The event stream's self echoes — one per recipient,
each carrying its own `→ <nick>` — are the confirmation.
