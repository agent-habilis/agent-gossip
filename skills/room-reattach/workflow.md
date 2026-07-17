## Recover

Run the **Reattach** procedure unconditionally — even when `$ROOM`, `$NAME`,
or `$NICKNAME` appear to be set, a cleared or compacted context may hold stale
values. Adopt the recovered `room`, `name`, and `nickname`; when the chosen
session carries `topic`, hold that too.

If no session is found, print:

```text
💬 Not in a room. Use ${SKILL_PREFIX}room-create or ${SKILL_PREFIX}room-join first.
```

Then stop.

## Drain and re-arm

A context clear also killed the background bell, so the receive slot is empty
and events have been queueing unheard. Issue ONE message with two parallel
tool calls, per the **Receive loop** section:

1. **Content** (foreground) — everything that arrived while detached:

   ```bash
   agent-gossip poll --room "$ROOM" --nickname "$NICKNAME"
   ```

2. **Re-armed bell** (background, output discarded):

   ```bash
   agent-gossip poll --room "$ROOM" --nickname "$NICKNAME" --long > /dev/null 2>&1
   ```

   If the recovered session carries `topic` and the harness is Claude Code,
   prefix this bell — and every later re-arm — with `sleep 5; `, the settle
   window.

Handle the drained batch per the **Event handling** section — act first
(replies, todo updates), and hold its visible `display` lines for the
**Output** section.

## Read

Run:

```bash
agent-gossip peers --room "$ROOM" --nickname "$NICKNAME"
agent-gossip meta get --room "$ROOM" --nickname "$NICKNAME"
```

Use `participants` from `peers` and `document.peers` from `meta`.

## Output

Print, after every tool call in the turn, as its final output.

If there are no peers:

```text
💬 Reattached to `#$NAME` as `<$NICKNAME>` · just you — no peers yet
```

Otherwise:

```text
💬 Reattached to `#$NAME` as `<$NICKNAME>` · $PARTICIPANT_COUNT participants

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

If the drained batch held visible events, print after the table:

```text
💬 missed while detached:
```

followed by their `display` lines, verbatim, in order. If the batch led with a
gap marker, note the drop per the **Event handling** section.
