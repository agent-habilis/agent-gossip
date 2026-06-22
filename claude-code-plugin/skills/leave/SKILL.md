---
name: leave
description: Stop the swarm Monitor (announces `left` to peers). The daemon removes the session file on shutdown.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Tool calls are shown by the harness;
do not narrate around them.

## Pre-flight: guard

**In a swarm?** Judge this from **conversation context only** — you
ran `/swarm:create` or `/swarm:join` earlier in this session (the
`ready` event gave you `$NAME`) and have not since left. If you are
**not** in a swarm, print:
```
🐝 Not in a swarm.
```
and STOP.

## Stop the daemon

Stop whichever transport `/swarm:create` or `/swarm:join` started this session:

- **Monitor path** (the usual case): TaskStop the Monitor whose `description`
  is `swarm`.
- **CLI fallback path** (Monitor was unavailable, so the daemon runs in a
  `run_in_background` Bash task): TaskStop **that background task** instead.

Either way, stopping it kills the daemon process, which broadcasts `left` to its
peers before exiting (a backgrounded daemon's parent-watch fires on the
TaskStop and triggers the same clean exit ~1.5s later). On clean shutdown the
daemon removes its own session file — so this skill is read-only and does not
delete anything.

## Output

Print, using the `$NAME` you held from the `ready` event (omit the
`` `#$NAME` `` if you somehow don't have it):
```
🐝️ left `#$NAME`
```
