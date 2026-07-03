---
name: reply
description: Send a message addressed to a specific peer. First arg is the target nickname, rest is the body.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Tool calls are shown by the harness;
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

## Pre-flight: guard

If you hold `$SWARM`/`$NICKNAME` from a `/swarm:create` or `/swarm:join`
`ready` event this session, proceed. Otherwise try to reattach first:
follow `../shared/reattach.md` (resolved relative to this SKILL.md's
directory). Only if reattach also yields no swarm, print:
```
🐝 Not in a swarm. Use /swarm:create or /swarm:join first.
```
and STOP.

## Send the reply

`$SWARM`/`$NICKNAME` are from the `ready` event (copy the `🐝…` id
verbatim):

```bash
ahsw msg --swarm "$SWARM" --nickname "$NICKNAME" --text "$TEXT" --reply "$TARGET"
```

## Output

Produce **no output of your own**. Do not re-type or re-render `$TEXT`.

The Monitor started by `/swarm:create` or `/swarm:join` surfaces the
daemon's self-echo of this reply as a `msg` event with `"self":true`,
carrying the authoritative pre-built `display` line (with the `→` arrow to
`$TARGET`). That echo is the verbatim confirmation — emit its `display`
field per the create/join event handler, and nothing else here.
