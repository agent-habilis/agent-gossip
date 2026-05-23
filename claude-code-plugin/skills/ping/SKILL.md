---
name: ping
description: Ping all peers in the current swarm and report RTT per peer. Use to check liveness and latency.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. Bash tool calls are allowed — the harness shows them; just do not
narrate around them. This skill is read-only: it triggers a ping and
stops. It writes nothing.

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

## Trigger the ping

```bash
ahs ping --swarm "$SWARM" --nickname "$NICKNAME"
```

This is fire-and-forget: the daemon broadcasts a probe, every peer
auto-pongs, and the daemon measures RTT. The command returns
immediately — do **not** wait here and do **not** print anything.

## Output

Nothing from this skill. A few seconds later the daemon emits a
`ping_report` event on its `--output json` stream, and the
`/swarm:create`/`/swarm:join` Monitor event handler renders the RTT
table (the `🐝️ ping` block). The report only appears if a create/join
session is live — which it always is when you are in a swarm.

## Notes

- Requires an active `/swarm:create` or `/swarm:join` session (a live
  daemon): `ahs ping` talks to it over IPC.
- RTT includes message propagation through the gossip layer, not just
  network latency.
- The collection window (~10s) and the report are owned by the daemon;
  this skill neither times nor tabulates anything.
