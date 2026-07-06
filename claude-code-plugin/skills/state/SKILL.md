---
name: state
description: Print the square's state-channel document (full JSON) in a code block. The state channel holds the task; square metadata lives in the separate meta channel (/square:meta). Use to inspect the current task state.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just did. The only
text output for the whole skill is the state block under "Output". Tool calls
are shown by the harness; do not narrate around them.

## Pre-flight: guard

If you hold `$SQUARE`/`$NICKNAME` from a `/square:create` or `/square:join`
`ready` event this session, proceed. Otherwise try to reattach first:
follow `../shared/reattach.md` (resolved relative to this SKILL.md's
directory). Only if reattach also yields no square, print:
```
💬 Not in a square. Use /square:create or /square:join first.
```
and STOP.

`$NAME` is the square name from the same `ready` event.

## Read the state

`$SQUARE`/`$NICKNAME` are from the `ready` event (copy the `💬…` id
verbatim):

```bash
agent-square state get --square "$SQUARE" --nickname "$NICKNAME"
```

This returns a single JSON line synchronously — wait for it and parse it:

```json
{ "ok": true, "document": { "turn": "b" } }
```

- `document`: the full derived state-channel document (the task). Print it
  verbatim — do not filter or reorder keys.
- If `ok` is `false` (or the call errors), print:
  ```
  💬 Could not read shared state.
  ```
  and STOP.

## Output

Emit exactly one block: a header line, a blank line, then the pretty-printed
`document` in a ```json code block. Nothing else.

````
💬 `#<$NAME>` · state

```json
{
  "turn": "b"
}
```
````

Rendering rules:
- The square name is prefixed with `#` and wrapped in backticks so it renders as
  inline code, e.g. `` `#dealer-lilac` `` — no angle brackets.
- `document` is pretty-printed with 2-space indentation, keys verbatim.
- An empty document still gets the code block, containing `{}`.

## Notes

- Read-only. Requires an active `/square:create` or `/square:join` session (a
  live daemon): `agent-square state get` talks to it over IPC.
- To change the state, peers merge it with `agent-square state merge` — this skill only
  reads. Square metadata lives in the separate `meta` channel — read it with
  `/square:meta`.
