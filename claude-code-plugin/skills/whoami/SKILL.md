---
name: whoami
description: Print the local nickname assigned in the current swarm session.
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
NICKNAME=$(echo "$SESSION" | jq -r '.nickname // ""')
```

If `SWARM` is empty, print:
```
Not in a swarm. Use /swarm:create or /swarm:join first.
```
STOP.

## Output

Print:
```
🐝️ `<$NICKNAME>`
```
