## Recover

Run the **Reattach** procedure unconditionally — even when `$GOSSIP`, `$NAME`,
or `$NICKNAME` appear to be set, a cleared or compacted context may hold stale
values. Adopt the recovered `gossip`, `name`, and `nickname`; when the chosen
session carries `topic`, hold that too.

If no session is found, print:

```text
💬 not in a gossip. use ${SKILL_PREFIX}gossip-create or ${SKILL_PREFIX}gossip-join first.
```

Then stop.

## Drain

The **Reattach** procedure's **Bell guard** already checked whether the bell
survived the clear and re-armed exactly one when it was gone — arm nothing
further here.

Events that arrived while context was detached are still queued. Drain them
with one foreground poll — the daemon's read cursor advances, so repeating it
returns an empty array:

```bash
agent-gossip poll --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Handle the drained batch per the **Event handling** section — act first
(replies, todo updates), and hold its visible `display` lines for the
**Output** section.

## Read

Run:

```bash
agent-gossip peers --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Use `peers` for the peer count.

## Output

Print, after every tool call in the turn, as its final output.

If there are no peers:

```text
💬 context reattached for `#$NAME` as `<$NICKNAME>` · just you · no peers yet
```

Otherwise:

```text
💬 context reattached for `#$NAME` as `<$NICKNAME>` · $PEER_COUNT peers
```

If the drained batch held visible events, print after the reattach line:

```text
💬 missed while context was detached:
```

followed by their `display` lines, verbatim, in order. If the batch led with a
gap marker, note the drop per the **Event handling** section.
