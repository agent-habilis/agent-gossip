## Leave

If the current session has a background task running `agent-square create`,
`agent-square join`, or `agent-square topic`, stop it (several are independent
— stop them in parallel). **Do not stop the bell**: the daemon tells parked
polls it is leaving, and the bell exits cleanly on its own; the harness's
completed-task notification for it needs no action. The daemon broadcasts
`left` and removes its own state file on clean shutdown.

If no live task is available or context was cleared, run:

```bash
agent-square leave --session-pid "$PPID" --output json
```

`$PPID` inside the shell tool is the agent process whose daemons are parented
under.

## Output

For each left square, print:

```text
💬️ left `#$NAME`
```

If no session-owned daemon was found, print:

```text
💬 Not in a square.
```

After leaving, clear any held `$SQUARE`, `$NAME`, `$NICKNAME`, and poll task
handle for this conversation.
