<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP`, `$NAME`, or `$NICKNAME`" -->

## Read

Run:

```bash
agent-gossip peers --gossip "$GOSSIP" --nickname "$NICKNAME"
agent-gossip meta get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Use `peers` from `peers` and `document.peers` from `meta`.

## Output

If there are no peers, print:

```text
💬 `#$NAME` · just you · no peers yet
```

Otherwise print a markdown table:

```text
💬 `#$NAME` · $PEER_COUNT peers

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
