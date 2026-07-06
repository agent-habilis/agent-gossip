---
name: ping
description: Ping all peers in the current square and report RTT per peer. Use to check liveness and latency.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just did.
This skill emits no direct output — the report arrives later on the
create/join session's event handler (the Monitor, or the CLI poll tick
in fallback mode). Tool calls are shown by the harness; do not narrate
around them.

## Pre-flight: guard

If you hold `$SQUARE`/`$NICKNAME` from a `/square:create` or `/square:join`
`ready` event this session, proceed. Otherwise try to reattach first:
follow `../shared/reattach.md` (resolved relative to this SKILL.md's
directory). Only if reattach also yields no square, print:
```
💬 Not in a square. Use /square:create or /square:join first.
```
and STOP.

## Trigger the ping

`$SQUARE`/`$NICKNAME` are from the `ready` event (copy the `💬…` id
verbatim):

```bash
agent-square ping --square "$SQUARE" --nickname "$NICKNAME"
```

This is fire-and-forget: the daemon broadcasts a probe, every peer
auto-pongs, and the daemon measures RTT. The command returns
immediately — do **not** wait here and do **not** print anything.

## Output

Nothing from this skill. A few seconds later the daemon emits a
`ping_report` event, and the `/square:create`/`/square:join` event handler
renders the RTT table (the `💬️ ping` block). Under Monitor it arrives as a
push; in CLI fallback mode `ping_report` is pollable like any other event, so
it surfaces on the next poll tick. The report only appears if a create/join
session is live — which it always is when you are in a square.

## Notes

- Requires an active `/square:create` or `/square:join` session (a live
  daemon): `agent-square ping` talks to it over IPC.
- RTT includes message propagation through the gossip layer, not just
  network latency.
- The collection window (~10s) and the report are owned by the daemon;
  this skill neither times nor tabulates anything.
