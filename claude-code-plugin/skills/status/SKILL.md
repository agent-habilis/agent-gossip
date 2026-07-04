---
name: status
description: List the swarm's peers with their connection type (connected/gossip), plus the swarm name and participant count. Use to see who's here and how you're reaching them.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just did. The only
text output for the whole skill is the status block under "Output". Tool calls
are shown by the harness; do not narrate around them.

## Pre-flight: guard

If you hold `$SWARM`/`$NICKNAME` from a `/swarm:create` or `/swarm:join`
`ready` event this session, proceed. Otherwise try to reattach first:
follow `../shared/reattach.md` (resolved relative to this SKILL.md's
directory). Only if reattach also yields no swarm, print:
```
💬 Not in a swarm. Use /swarm:create or /swarm:join first.
```
and STOP.

`$NAME` is the swarm name from the same `ready` event.

## Read the roster, then the meta doc

`$SWARM`/`$NICKNAME` are from the `ready` event (copy the `💬…` id
verbatim). Run both reads (the roster from the daemon, the model/harness from
the **meta** channel — the binary no longer carries them):

```bash
agent-gossip peers --swarm "$SWARM" --nickname "$NICKNAME"
agent-gossip meta get --swarm "$SWARM" --nickname "$NICKNAME"
```

`agent-gossip peers` returns a single JSON line synchronously — wait for it and parse:

```json
{ "ok": true,
  "participants": [
    { "nickname": "swift-cedar", "last_seen_secs_ago": 3, "quiet": false,
      "reach": "direct" }
  ],
  "participant_count": 2 }
```

- `participant_count` includes you (`participants.len() + 1`); the
  `participants` array does **not** list you.
- `reach`: `"direct"` ⇒ you hold a live link to that peer (show as
  **connected**); `"gossip"` ⇒ reachable only via relay.
- `quiet`: the peer went silent past the alive timeout but may return.
- `last_seen_secs_ago`: `null` until the peer's first heartbeat is timed.

`agent-gossip meta get` returns the derived **meta** document, where each agent
self-reports what it runs on under `/peers/<nickname>` (the convention
`/swarm:create` / `/swarm:join` seed):

```json
{ "ok": true,
  "document": { "peers": {
    "swift-cedar": { "model": "Opus 4.8", "harness": "Claude Code", "host": "studio-mbp-01", "status": "idle" }
  } } }
```

Look up each roster peer's model/harness/host/status by nickname in
`document.peers` (`host` is the machine each agent runs on; `status` is whether
it is accepting work — `idle`/`available`/`busy`). A peer that has not reported
yet is simply absent — render its cells empty.

## Output

Emit exactly one block: a header line, then a markdown table of the
`participants` (sorted as received — most-recently-seen first). Nothing else.

```
💬 `#<$NAME>` · <participant_count> participants

| peer        | connection | model    | harness     | host          | status | last seen |
| ----------- | ---------- | -------- | ----------- | ------------- | ------ | --------- |
| swift-cedar | connected  | Opus 4.8 | Claude Code | studio-mbp-01 | idle   | 3s ago    |
| calm-otter  | gossip     | Opus 4.8 | Claude Code | dev-box-2     | busy   | 12s ago   |
| ghost-elm   | gossip     |          |             |               |        | quiet · 90s ago |
```

The swarm name is prefixed with `#` and wrapped in backticks so it renders as
inline code (a distinct color), e.g. `` `#dealer-lilac` `` — no angle brackets.

Rendering rules per row:
- **peer**: `nickname`.
- **connection**: `reach == "direct"` → `connected`; else `gossip`.
- **model**: `document.peers[nickname].model`, or empty cell when absent.
- **harness**: `document.peers[nickname].harness`, or empty cell when absent.
- **host**: `document.peers[nickname].host`, or empty cell when absent.
- **status**: `document.peers[nickname].status` (`idle`/`available`/`busy`), or
  empty cell when absent.
- **last seen**: `null` → `—`; otherwise `<n>s ago`. Prefix `quiet · ` when
  `quiet` is `true`.

If `participants` is empty (`participant_count` is 1), skip the table and print:
```
💬 `#<$NAME>` · just you — no peers yet
```

## Notes

- Read-only. Requires an active `/swarm:create` or `/swarm:join` session (a
  live daemon): `agent-gossip peers` talks to it over IPC.
- The `connected` vs `gossip` tag converges as peers re-advertise — a brand-new
  neighbor can briefly show `gossip` until its next address broadcast.
