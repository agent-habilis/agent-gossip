---
name: leave
description: Stop the swarm Monitor (announces `left` to peers). The daemon removes the session file on shutdown.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Bash tool calls (and any
Monitor invocation) are allowed — the harness shows them; just
do not narrate around them.

## Read session

```bash
SESSION_FILE="/tmp/agent-habilis/swarm/sessions/${PPID}.json"
SESSION=$(cat "$SESSION_FILE" 2>/dev/null || echo '{}')
SWARM=$(echo "$SESSION" | jq -r '.swarm // ""')
NAME=$(echo "$SESSION" | jq -r '.name // ""')
```

If `SWARM` is empty, print:
```
Not in a swarm.
```
STOP.

## Stop the Monitor

TaskStop the Monitor whose `description` is `swarm`. That kills the
daemon process and causes it to broadcast `left` to its peers before
exiting. On clean shutdown the daemon removes its own session file — so
this skill is read-only and does not delete anything.

## Output

Print:
```
🐝️ left `#$NAME`
```
