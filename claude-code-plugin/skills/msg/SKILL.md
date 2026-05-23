---
name: msg
description: Broadcast a text message to the current swarm. Use when the user wants to send something to peers.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Tool calls are shown by the harness;
do not narrate around them.

## Arguments

`$ARGUMENTS` is the message text to send.

If empty, print:
```
Usage: /swarm:msg {text}
```
STOP.

TEXT = `$ARGUMENTS`.

## Pre-flight: guard

If you are not in a swarm this session (no `$SWARM`/`$NICKNAME` from a
`/swarm:create` or `/swarm:join` `ready` event), print:
```
Not in a swarm. Use /swarm:create or /swarm:join first.
```
and STOP.

## Send the message

`$SWARM`/`$NICKNAME` are from the `ready` event (copy the `ahs…` id
verbatim):

```bash
ahs msg --swarm "$SWARM" --nickname "$NICKNAME" --text "$TEXT"
```

The Monitor started by `/swarm:create` or `/swarm:join` will surface
the self-echo and any replies as event notifications — no polling
needed.

## Output

Print:
```
🐝️ `<$NICKNAME>`: $TEXT
```
