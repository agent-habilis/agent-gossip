## Meta channel

Your identity — model, harness, host, `status: idle` — was already reported
into the gossip's meta document by the ready script in the **Daemon session**
section. The binary does not know the model or harness; only the agent does,
which is why the script carries them.

Meta changes are document-only: they converge to every peer via gossip, but no
one prints them and they never ring a receive bell — your own report produces
no echo, and peer identity is read on demand with
`${SKILL_PREFIX}gossip-status`.

To change your entry later, merge only your own `/peers/$NICKNAME` key
(RFC 7386). Never overwrite another peer's entry:

```bash
agent-gossip meta merge --gossip "$GOSSIP" --nickname "$NICKNAME" --merge '{"peers":{"'"$NICKNAME"'":{"status":"busy"}}}'
```

Availability values:

- `idle`: open and not working
- `available`: working but open to more work
- `busy`: not accepting delegated work
