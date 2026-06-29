---
name: state
description: Print the swarm's state-channel document (full JSON) in a code block, with its doc hash. The state channel holds the task; swarm metadata lives in the separate meta channel (/swarm:meta). Use to inspect the current task state.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just did. The only
text output for the whole skill is the state block under "Output". Tool calls
are shown by the harness; do not narrate around them.

## Pre-flight: guard

If you are not in a swarm this session (no `$SWARM`/`$NICKNAME` from a
`/swarm:create` or `/swarm:join` `ready` event), print:
```
🐝 Not in a swarm. Use /swarm:create or /swarm:join first.
```
and STOP.

`$NAME` is the swarm name from the same `ready` event.

## Read the state

`$SWARM`/`$NICKNAME` are from the `ready` event (copy the `🐝…` id
verbatim):

```bash
ahsw state get --swarm "$SWARM" --nickname "$NICKNAME"
```

This returns a single JSON line synchronously — wait for it and parse it:

```json
{ "ok": true, "document": { "turn": "b" }, "doc_hash": "9f2c1ab3…" }
```

- `document`: the full derived state-channel document (the task). Print it
  verbatim — do not filter or reorder keys.
- `doc_hash`: SHA256 of the document, used for compare-and-set patches.
- If `ok` is `false` (or the call errors), print:
  ```
  🐝 Could not read shared state.
  ```
  and STOP.

## Output

Emit exactly one block: a header line, a blank line, then the pretty-printed
`document` in a ```json code block. Nothing else.

````
🐝 `#<$NAME>` · state · hash `<doc_hash first 12 chars>`

```json
{
  "turn": "b"
}
```
````

Rendering rules:
- The swarm name is prefixed with `#` and wrapped in backticks so it renders as
  inline code, e.g. `` `#dealer-lilac` `` — no angle brackets.
- `hash` shows the first 12 characters of `doc_hash`, wrapped in backticks.
- `document` is pretty-printed with 2-space indentation, keys verbatim.
- An empty document still gets the code block, containing `{}`.

## Notes

- Read-only. Requires an active `/swarm:create` or `/swarm:join` session (a
  live daemon): `ahsw state get` talks to it over IPC.
- To change the state, peers patch it with `ahsw state patch` — this skill only
  reads. Swarm metadata lives in the separate `meta` channel — read it with
  `/swarm:meta`.
