---
name: reply
description: Send a message addressed to a specific peer. First arg is the target nickname, rest is the body.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Bash tool calls (and any
Monitor invocation) are allowed — the harness shows them; just
do not narrate around them.

## Arguments

`$ARGUMENTS` should be `{nickname} {text}` — the first whitespace-
delimited token is the target peer's nickname; the rest is the reply
text.

If empty or just one token:
```
Usage: /swarm:reply {nickname} {text}
```
STOP.

TARGET = first token of `$ARGUMENTS`
TEXT = remainder of `$ARGUMENTS`

## Read session

```bash
SESSION_FILE="/tmp/agent-habilis-swarm/sessions/${PPID}.json"
SESSION=$(cat "$SESSION_FILE" 2>/dev/null || echo '{}')
SWARM=$(echo "$SESSION" | jq -r '.swarm // ""')
NICKNAME=$(echo "$SESSION" | jq -r '.nickname // ""')
```

If `SWARM` is empty, print:
```
Not in a swarm. Use /swarm:create or /swarm:join first.
```
STOP.

## Validate ASCII

The body must be ASCII only. If `$TEXT` contains non-ASCII characters,
print `Message body must be ASCII only.` and STOP.

## Send the reply

```bash
agent-habilis-swarm msg --swarm "$SWARM" --nickname "$NICKNAME" --text "$TEXT" --reply "$TARGET"
```

The Monitor started by `/swarm:create` or `/swarm:join` will surface
the self-echo as an event notification — no polling needed.

## Output

Print:
```
🐝️ `<$NICKNAME>` → `<$TARGET>`: $TEXT
```
