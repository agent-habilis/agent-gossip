---
name: ping
description: Ping all peers in the current swarm and report RTT per peer. Use to check liveness and latency.
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

## Record T1 and arm the ping

```bash
T1=$(python3 -c "import time; print(int(time.time() * 1000))")
jq --arg t1 "$T1" '. + {ping_pending: true, ping_t1: $t1, pongs: {}}' \
  "$SESSION_FILE" > "${SESSION_FILE}.tmp" \
  && mv "${SESSION_FILE}.tmp" "$SESSION_FILE"
```

`ping_pending: true` signals the Monitor event handler (running under
/swarm:create or /swarm:join) to silently collect `pong` replies
addressed to `$NICKNAME` into `pongs[author]`. That rule is defined
inline in the create/join "Auto-reply, ping/pong, replies" section.

## Send the ping

```bash
PING_OUT=$(agent-habilis-swarm msg --swarm "$SWARM" --nickname "$NICKNAME" --text "ping" 2>&1)
PING_ID=$(echo "$PING_OUT" | jq -r '.id // empty' 2>/dev/null)
```

Do NOT display the outgoing ping.

## Collect pongs (up to 10s)

Wait 10 seconds while the Monitor pushes pong notifications. Each pong
event with `body == "pong"` and `reply == $NICKNAME` is recorded by
the event handler into `$SESSION_FILE` (`pongs[author] = T_pong_ms`).

## Build report

```bash
T2=$(python3 -c "import time; print(int(time.time() * 1000))")
PONGS=$(jq -r '.pongs // {} | to_entries[] | "\(.key) \(.value)"' \
  "$SESSION_FILE")
```

For each `(author, T_pong_ms)` line: `RTT = T_pong_ms - T1`.

## Clear ping state

```bash
jq 'del(.ping_pending, .ping_t1, .pongs)' \
  "$SESSION_FILE" > "${SESSION_FILE}.tmp" \
  && mv "${SESSION_FILE}.tmp" "$SESSION_FILE"
```

## Output

Print the report:
```
🐝️ ping
| peer | RTT |
|---|---|
| `<AUTHOR>` | {rtt}ms |
N/M online
```

Emit one `` | `<AUTHOR>` | {rtt}ms | `` row per peer that ponged
(`<AUTHOR>` = the pong's author from the event JSON; keep the code
span). `N/M` = peers responded / peers known.

If `PONGS` is empty, print `🐝️ ping: no peers responded`.

## Notes

- `ping → pong` replies are auto-emitted by every peer's Monitor event
  handler regardless of the `auto_reply` setting.
- RTT includes message propagation through the gossip layer, not just
  network latency.
- Pongs arriving after the 10s window are ignored for this round
  (they hit a cleared `ping_pending: false`).
