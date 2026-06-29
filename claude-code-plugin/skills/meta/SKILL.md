---
name: meta
description: Print the swarm's meta-channel document (full JSON) in a code block, with its doc hash. The meta channel is a second shared state, by convention holding swarm metadata (peer info, capabilities). Use to inspect current swarm metadata.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just did. The only
text output for the whole skill is the meta block under "Output". Tool calls
are shown by the harness; do not narrate around them.

## Pre-flight: guard

If you are not in a swarm this session (no `$SWARM`/`$NICKNAME` from a
`/swarm:create` or `/swarm:join` `ready` event), print:
```
🐝 Not in a swarm. Use /swarm:create or /swarm:join first.
```
and STOP.

`$NAME` is the swarm name from the same `ready` event.

## Read the meta channel

The `meta` channel is a second shared-state document, byte-for-byte the same
machinery as `state` (the task channel) — independent log, document, and hash.
By convention it holds swarm metadata (each peer's model/harness, capabilities),
while `state` holds the task. The daemon does not differentiate them.

`$SWARM`/`$NICKNAME` are from the `ready` event (copy the `🐝…` id verbatim):

```bash
ahsw meta get --swarm "$SWARM" --nickname "$NICKNAME"
```

This returns a single JSON line synchronously — wait for it and parse it:

```json
{ "ok": true,
  "document": { "peers": { "lava-phase": { "model": "Opus 4.8" } } },
  "doc_hash": "9f2c1ab3…" }
```

- `document`: the full derived meta-channel document. Print it verbatim — do not
  filter or reorder keys.
- `doc_hash`: SHA256 of the document, used for compare-and-set patches
  (independent of the `state` channel's hash).
- If `ok` is `false` (or the call errors), print:
  ```
  🐝 Could not read the meta channel.
  ```
  and STOP.

## Output

Emit exactly one block: a header line, a blank line, then the pretty-printed
`document` in a ```json code block. Nothing else.

````
🐝 `#<$NAME>` · meta · hash `<doc_hash first 12 chars>`

```json
{
  "peers": {
    "lava-phase": {
      "model": "Opus 4.8"
    }
  }
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
  live daemon): `ahsw meta get` talks to it over IPC.
- To change the meta channel, peers patch it with `ahsw meta patch` — this skill
  only reads. The `state` channel (the task) is read with `/swarm:state`.
