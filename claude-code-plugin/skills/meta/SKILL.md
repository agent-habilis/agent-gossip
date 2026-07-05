---
name: meta
description: Print the swarm's meta-channel document (full JSON) in a code block. The meta channel is a second shared state, by convention holding swarm metadata (peer info, capabilities). Use to inspect current swarm metadata.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just did. The only
text output for the whole skill is the meta block under "Output". Tool calls
are shown by the harness; do not narrate around them.

## Pre-flight: guard

If you hold `$SWARM`/`$NICKNAME` from a `/gossip:create` or `/gossip:join`
`ready` event this session, proceed. Otherwise try to reattach first:
follow `../shared/reattach.md` (resolved relative to this SKILL.md's
directory). Only if reattach also yields no swarm, print:
```
💬 Not in a swarm. Use /gossip:create or /gossip:join first.
```
and STOP.

`$NAME` is the swarm name from the same `ready` event.

## Read the meta channel

The `meta` channel is a second shared-state document, byte-for-byte the same
machinery as `state` (the task channel) — independent log and document.
By convention it holds swarm metadata (each peer's model/harness/host, capabilities),
while `state` holds the task. The daemon does not differentiate them.

`$SWARM`/`$NICKNAME` are from the `ready` event (copy the `💬…` id verbatim):

```bash
agent-gossip meta get --swarm "$SWARM" --nickname "$NICKNAME"
```

This returns a single JSON line synchronously — wait for it and parse it:

```json
{ "ok": true,
  "document": { "peers": { "lava-phase": { "model": "Opus 4.8" } } } }
```

- `document`: the full derived meta-channel document. Print it verbatim — do not
  filter or reorder keys.
- If `ok` is `false` (or the call errors), print:
  ```
  💬 Could not read the meta channel.
  ```
  and STOP.

## Output

Emit exactly one block: a header line, a blank line, then the pretty-printed
`document` in a ```json code block. Nothing else.

````
💬 `#<$NAME>` · meta

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
- `document` is pretty-printed with 2-space indentation, keys verbatim.
- An empty document still gets the code block, containing `{}`.

## Notes

- Read-only. Requires an active `/gossip:create` or `/gossip:join` session (a
  live daemon): `agent-gossip meta get` talks to it over IPC.
- To change the meta channel, peers merge it with `agent-gossip meta merge` — this skill
  only reads. The `state` channel (the task) is read with `/gossip:state`.
