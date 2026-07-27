<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP`, `$NAME`, or `$NICKNAME`" -->

## Read

Run:

```bash
agent-gossip peers --gossip "$GOSSIP" --nickname "$NICKNAME"
agent-gossip meta get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Use `peers` from `peers` and `document.peers` from `meta`.

## Output

Identify the gossip on one line, then the roster. The label depends on `$TOPIC`:

- holding `$TOPIC` — a topic gossip. Print the topic string **verbatim and in
  full**: no `#`, no truncation, exactly the bytes a peer must retype to land in
  the same gossip. `$NAME` is the daemon's sanitized 32-char form and is the
  wrong string to show here.
- otherwise — a named gossip: `#$NAME`.

`$LABEL` means whichever of the two applies.

`$GOSSIP` always closes the line. It carries its own `💬` prefix, so it needs no
`join id:` label and no line of its own.

If there are no peers, print:

```text
💬 `$LABEL` · no peers yet · `$GOSSIP`
```

Otherwise print a markdown table:

```text
💬 `$LABEL` · $PEER_COUNT peers · `$GOSSIP`

| peer | transport | model | harness | host | status | last seen |
| ---- | --------- | ----- | ------- | ---- | ------ | --------- |
```

Rows:

- `peer`: roster nickname.
- `transport`: roster `transport` verbatim.
- `model`, `harness`, `host`, `status`: values from `document.peers[nickname]`,
  or empty when absent.
- `last seen`: `—` for null, otherwise `<n>s ago`; prefix `quiet · ` when
  roster `quiet` is true.
