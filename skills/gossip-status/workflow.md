<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP`, `$NAME`, or `$NICKNAME`" -->

## Read

Run:

```bash
agent-gossip peers --gossip "$GOSSIP" --nickname "$NICKNAME"
agent-gossip meta get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Use `peers` from `peers` and `document.peers` from `meta`. The roster never
lists yourself; your own row comes from `document.peers[$NICKNAME]`.

The roster chains the quiet peers onto the active ones, so its length is not
the gossip's live size. Two counts come from it:

- `$PEER_COUNT` — entries whose `quiet` is `false`, plus one for yourself.
- `$QUIET_COUNT` — entries whose `quiet` is `true`.

Do not use the response's `peer_count`, which includes self.

## Output

Identify the gossip on one line, then the roster. The label depends on `$TOPIC`:

- holding `$TOPIC` — a topic gossip. Print the topic string **verbatim and in
  full**: no `#`, no truncation, exactly the bytes a peer must retype to land in
  the same gossip. `$NAME` is the daemon's sanitized 32-char form and is the
  wrong string to show here.
- otherwise — a named gossip: `#$NAME`.

`$LABEL` means whichever of the two applies.

`$GOSSIP` always closes the line, labelled `join`, so it needs no line of its
own. The hash is bare base58 with no prefix, so the label is what tells a
reader what the trailing token is.

Always print a markdown table, even when the roster is empty — your own row is
always in it. Quiet peers are listed too, because a quiet peer can come back
and stays addressable:

```text
💬 `$LABEL` · $PEER_COUNT peers · join `$GOSSIP`

| peer | transport | model | harness | host | cwd | status | last seen |
| ---- | --------- | ----- | ------- | ---- | --- | ------ | --------- |
```

When `$QUIET_COUNT` is above zero, the tally follows the count:

```text
💬 `$LABEL` · $PEER_COUNT peers · $QUIET_COUNT quiet · join `$GOSSIP`
```

The first row is always yourself:

- `peer`: `$NICKNAME (you)`.
- `transport` and `last seen`: `—`.
- `model`, `harness`, `host`, `cwd`, `status`: values from
  `document.peers[$NICKNAME]`, or empty when absent.

Then one row per roster entry, in roster order:

- `peer`: roster nickname.
- `transport`: roster `transport` verbatim.
- `model`, `harness`, `host`, `cwd`, `status`: values from `document.peers[nickname]`,
  or empty when absent.
- `last seen`: `—` for null, otherwise `<n>s ago`; prefix `quiet · ` when
  roster `quiet` is true.
