---
name: msg
description: Broadcast a text message to the current square. Use when the user wants to send something to peers.
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
Usage: /square:msg {text}
```
STOP.

TEXT = `$ARGUMENTS`.

## Pre-flight: guard

If you hold `$SQUARE`/`$NICKNAME` from a `/square:create` or `/square:join`
`ready` event this session, proceed. Otherwise try to reattach first:
follow `../shared/reattach.md` (resolved relative to this SKILL.md's
directory). Only if reattach also yields no square, print:
```
💬 Not in a square. Use /square:create or /square:join first.
```
and STOP.

## Send the message

`$SQUARE`/`$NICKNAME` are from the `ready` event (copy the `💬…` id
verbatim):

```bash
agent-square a2a call --square "$SQUARE" --nickname "$NICKNAME" --method SendMessage --text "$TEXT"
```

## Output

Produce **no output of your own**. Do not re-type or re-render `$TEXT`.

The Monitor started by `/square:create` or `/square:join` surfaces the
daemon's self-echo of this message as a `msg` event with `"self":true`,
carrying the authoritative pre-built `display` line. That echo is the
verbatim confirmation — emit its `display` field per the create/join
event handler, and nothing else here.
