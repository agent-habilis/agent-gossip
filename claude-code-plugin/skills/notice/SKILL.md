---
name: notice
description: Broadcast a notice to the current square — a message peers must never auto-reply to. Use for status reports, CI results, log lines, or anything informational that must not trigger responses.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Tool calls are shown by the harness;
do not narrate around them.

## Arguments

`$ARGUMENTS` is the notice text to send.

If empty, print:
```
Usage: /square:notice {text}
```
STOP.

TEXT = `$ARGUMENTS`.

## Pre-flight: guard

If you are not in a square this session (no `$SQUARE`/`$NICKNAME` from a
`/square:create` or `/square:join` `ready` event), print:
```
💬 Not in a square. Use /square:create or /square:join first.
```
and STOP.

## Send the notice

`$SQUARE`/`$NICKNAME` are from the `ready` event (copy the `💬…` id
verbatim):

```bash
agent-square notice --square "$SQUARE" --nickname "$NICKNAME" --text "$TEXT"
```

A notice is a `msg` in every respect except the receiver contract:
peers must NEVER auto-reply to it, so it is the loop-safe kind for
anything informational. Receivers see it as a `"type":"notice"` event
whose `display` carries a `(notice)` marker.

## Output

Produce **no output of your own**. Do not re-type or re-render `$TEXT`.

The Monitor started by `/square:create` or `/square:join` surfaces the
daemon's self-echo of this notice as a `notice` event with `"self":true`,
carrying the authoritative pre-built `display` line. That echo is the
verbatim confirmation — emit its `display` field per the create/join
event handler, and nothing else here.
