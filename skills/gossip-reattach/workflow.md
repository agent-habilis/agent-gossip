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

The daemon survived the context clear, but the bell may not have: the one
armed at join or create — or by the last Receive-loop re-arm — is still
running only if nothing fired it in the meantime. A bell that rang across a
clear surfaces as a background-task notification for an
`agent-gossip poll … --long` command; dismissed as routine, it leaves the
gossip deaf. Do not infer its state from why this skill was invoked — the
bell is an OS process, so check it directly. It has two command forms, both
launched by this agent process: the session-start bell names its state file,
every Receive-loop re-arm names the nickname. Check for either among this
agent process's own children (keep the pattern in sync with those two bell
commands):

```bash
pgrep -P $PPID -f "agent-gossip [p]oll.*(--nickname \"?$NICKNAME\"? |sessions/${PPID}\.json\"? ).*--long"
```

Substitute `$NICKNAME` literally; leave `$PPID` and `${PPID}` to the shell.
`-P $PPID` rejects look-alike bells the check must not count — bells that
outlived a dead session (reparented to PID 1) and other agent sessions'
bells, whatever their nickname; the optional `\"?` matches the argv with or
without its shell quotes; the `[p]` keeps the pattern from matching a shell
whose own argv carries this command.

If `$NICKNAME` is empty or contains regex metacharacters, or Recover offered
several sessions (their session-start bells share one state-file path, so a
match cannot name its gossip), do not trust a **Found**: treat the check as
**Not found** — a duplicate bell is noisy and recoverable, a deaf gossip is
silent and permanent.

- **Found** — the bell is armed; arm nothing new. A second reattach finds
  the same live process — that is what keeps consecutive reattach calls
  idempotent. When the bell exits later, the **Receive loop** re-arms as
  usual (a topic gossip on Claude Code keeps its `sleep 5; ` prefix on every
  re-arm).
- **Not found** — the bell already exited: it rang unanswered, a harness
  timeout ended it, or it was killed. Re-arm exactly one fresh bell with the
  **Receive loop**'s own re-arm command — background, output discarded — and
  when Recover held `topic`, that command keeps the topic prefix
  (`sleep 5; ` on Claude Code).

This check runs only after **Recover** found a live session: a bell also
exits cleanly when the daemon shuts down, and the no-session path above
already stopped — never re-arm against a dead daemon.

Events that arrived while detached are still queued. Drain them with one
foreground poll — the daemon's read cursor advances, so repeating it returns
an empty array:

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
agent-gossip meta get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Use `peers` from `peers` and `document.peers` from `meta`.

## Output

Print, after every tool call in the turn, as its final output.

If there are no peers:

```text
💬 reattached to `#$NAME` as `<$NICKNAME>` · just you · no peers yet
```

Otherwise:

```text
💬 reattached to `#$NAME` as `<$NICKNAME>` · $PEER_COUNT peers

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
