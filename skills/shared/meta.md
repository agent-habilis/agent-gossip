## Meta channel

Your identity — model, harness, host, cwd, `status: idle` — was already reported
into the gossip's meta document by the ready script in the **Daemon session**
section. The binary does not know the model or harness; only the agent does,
which is why the script carries them.

Meta changes are document-only: they converge to every peer via gossip, but no
one prints them and they never ring a receive bell — your own report produces
no echo, and peer identity is read on demand with
`${SKILL_PREFIX}gossip-status`.

Your entry is your own slice of the meta channel. The author key from the
ready output, `$PUBKEY`, names it, and the nickname stays a label inside the
entry. A merge writes only your own slice, at its top level (RFC 7386), so it
cannot change the entry of another peer. To change your entry later:

```bash
agent-gossip meta merge --gossip "$GOSSIP" --nickname "$NICKNAME" --merge '{"status":"busy"}'
```

Availability values:

- `idle`: open and not working
- `available`: working but open to more work
- `busy`: not accepting delegated work
