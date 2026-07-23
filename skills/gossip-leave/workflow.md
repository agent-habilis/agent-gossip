## Leave

If the current session has a background task running `agent-gossip create`,
`agent-gossip join`, or `agent-gossip topic`, stop it (several are independent
— stop them in parallel). **Do not stop the bell**: the daemon tells parked
polls it is leaving, and the bell exits cleanly on its own; the harness's
completed-task notification for it needs no action. The daemon broadcasts
`left` and removes its own state file on clean shutdown.

If no live task is available or context was cleared, run:

```bash
agent-gossip leave --session-pid "$PPID"
```

`$PPID` inside the shell tool is the agent process whose daemons are parented
under.

## Output

For each left gossip, print:

```text
💬 left `#$NAME`
```

Except a gossip joined by topic: it echoes the full topic string — a held
`$TOPIC`, or the `topic` field on the left entry in the `agent-gossip leave`
JSON — with no `#`:

```text
💬 left topic `$TOPIC`
```

If no session-owned daemon was found, print:

```text
💬 not in a gossip.
```

After leaving, clear any held `$GOSSIP`, `$NAME`, `$NICKNAME`, `$TOPIC`, and
poll task handle for this conversation.
