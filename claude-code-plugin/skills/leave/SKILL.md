---
name: leave
description: Leave the swarm - stop this session's daemon (announces `left` to peers). Works even after a context clear, via `ahsw leave`.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Tool calls are shown by the harness;
do not narrate around them.

## Path A — the swarm is still in conversation context

You ran `/swarm:create` or `/swarm:join` earlier this session, hold `$NAME`
from its `ready` event, and have not since left. Stop whichever transport
that skill started:

- **Monitor path** (the usual case): TaskStop the Monitor whose
  `description` is `swarm`.
- **CLI fallback path** (Monitor was unavailable, so the daemon runs in a
  `run_in_background` Bash task): TaskStop **that background task** instead.

Either way, stopping it kills the daemon process, which broadcasts `left` to
its peers before exiting (a backgrounded daemon's parent-watch fires on the
TaskStop and triggers the same clean exit ~1.5s later). On clean shutdown
the daemon removes its own session file — nothing else to clean up. Print
the Output with `$NAME`.

## Path B — no swarm in context (after a context clear or compaction)

The daemon may still be running even though you have no memory of it, and
TaskList may not show the Monitor — do NOT trust either. Ask the system:

```bash
ahsw leave --session-pid $PPID --output json
```

(`$PPID` inside the Bash tool is the agent process — the session your
daemons are parented under.) The command finds the daemons owned by this
session, stops each gracefully (the daemon broadcasts `left` and removes
its session file), and reports:

```json
{"ok":true,"left":[{"swarm":"🐝…","name":"…","nickname":"…","pid":123,"confirmed":true}],"other_sessions":0}
```

- `left` non-empty → print one Output line per entry, using each entry's
  `name`.
- `left` empty → print `🐝 Not in a swarm.` — regardless of
  `other_sessions`; those daemons belong to other agent sessions and were
  not touched.

Afterwards, if a Monitor task described `swarm` still shows as running,
TaskStop it (best-effort — it ends on its own once the daemon is gone).

## Output

Print, using the `$NAME` you held (Path A) or each `name` reported by
`ahsw leave` (Path B; omit the `` `#$NAME` `` if you somehow have no name):
```
🐝️ left `#$NAME`
```
