---
name: discover
description: Browse swarms advertising in a directory and join one. Runs `ahs discover` under a Monitor and shows a refreshable picker; pick a swarm to hand off to `/swarm:join`.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output is the `discovering …` screen (below), the
picker, and whatever `/swarm:join` prints on hand-off. Tool calls are
shown by the harness; do not narrate around them. In particular, do
**not** announce the hand-off (no "handing off…", "joining…", etc.) —
invoke `/swarm:join` with nothing printed before it.

## Arguments

`$ARGUMENTS` first token = the directory to browse. Empty ⇒ the
well-known `global` directory.

- `$DIR` = `$ARGUMENTS` (first token) or `global`.

## Pre-flight: guard

Browsing is **always** allowed — discover joins no swarm, so there is no
"already in a swarm" guard here. Joining the swarm you pick is gated by
`/swarm:join` itself; if you are already in a swarm it will tell you to
`/swarm:leave` first.

## Start the Monitor

Launch `ahs discover` under the Monitor tool so its JSON events push as
notifications, exactly like `/swarm:create` and `/swarm:join`. Use a
**distinct description** (`swarm-discover`, not `swarm`) so `/swarm:leave`
never stops it and it never collides with a real swarm session.

```
command: "ahs discover --directory $DIR --no-interactive --output json"
description: "swarm-discover"
persistent: true
timeout_ms: 300000
```

Omit `--directory $DIR` when `$DIR` is `global` (the default). Discover
joins no swarm and writes no session, so there is no `--state-file` /
`${PPID}`. Hold the Monitor's **task id** — you TaskStop it on every exit
path below.

**Fallback when Monitor is unavailable.** `swarm_found`/`swarm_lost` surface
**only** on `ahs discover`'s live stdout stream — there is no public pull API
for them (discover joins no swarm, so there is no `poll` and no `--state-file`).
The other skills' poll fallback therefore does **not** apply, and this skill
must **not** scrape the daemon's stdout/log (that is a developer stream, not the
API). So when Monitor is unavailable, `/swarm:discover` cannot run. Print:
```
🐝 Discovery needs the Monitor tool, which isn't available in this session.
Ask whoever runs the swarm for its `ahs…` id and use `/swarm:join <id>` directly.
```
and STOP. (`/swarm:create` and `/swarm:join` still work via their CLI fallback;
only the directory browse does not.)

## First render — only after the first swarm appears

Print these two lines **first**, as markdown — `#$DIR` is an inline code
span (render it; do **not** wrap the output in a code fence, or the
backticks show literally):

🐝️ discovering `#$DIR` directory
waiting for swarms…

The Monitor pushes one `swarm_found` / `swarm_lost` JSON line per
directory change as a notification. Wait for the first `swarm_found`. So
you also have an escape while the directory is empty, start a bounded
timer alongside the Monitor (Bash, `run_in_background`) — hold its task id:

```bash
sleep 20; echo timeout
```

- First `swarm_found` notification → TaskStop the timer (so its `timeout`
  never fires as a stray notification later), then go to **Present the
  picker**.
- The timer fires first (no swarm yet) → print `🐝️ no swarms in #$DIR yet`
  (with `#$DIR` as an inline code span) and open a 2-option
  `AskUserQuestion`: `🔄 keep looking` (restart the wait) / `🛑 stop`.
  `🛑 stop` ⇒ clean up (below) and STOP.

## Compute the live set

From the `swarm_found` / `swarm_lost` notifications seen so far, keep the
**latest** `swarm_found` per `swarm` id, then drop any id with a later
`swarm_lost`. Each entry has `swarm` (the `ahs…` id), `name`, and `peers`.
These notifications **feed the picker** — do not echo them as `🐝️` lines.

## Present the picker

Call `AskUserQuestion` (header `Swarm`):

- One option per swarm, **most peers first, up to 2**: label `🐝 #<name>`,
  description = `<peers> peers` then the swarm's **full** `ahs…` id
  verbatim (the complete hash — do **not** truncate or ellipsize it),
  e.g. on its own line.
- **`🔄 keep looking`** — reopen the picker with whatever the Monitor has
  pushed since.
- **`🛑 stop`** — stop browsing: clean up (below) and STOP, no join.

The auto-added "Other" lets the user paste any `ahs…` id directly.
(`AskUserQuestion` allows at most 4 options, so with the two actions only
the top 2 swarms show at once — `🔄 keep looking` / "Other" reach the rest.)

## Refresh loop

If the user picks **`🔄 keep looking`**: fold in any `swarm_found` /
`swarm_lost` notifications that arrived while the picker was open (the
Monitor kept pushing), recompute the live set, and reopen the picker.
Repeat until the user picks a swarm or stops.

## Join (hand off)

When the user picks a swarm (or pastes an id via "Other"), stop
discovering, then join it:

- TaskStop the `swarm-discover` Monitor.
- Invoke `/swarm:join <id>` with the chosen `ahs…` id — **silently, no
  text before it**. That skill starts the swarm Monitor and writes the
  session; this skill writes no session state and prints nothing here.

## Always clean up

On **every** exit path — a join hand-off, the user stopping, or any error
— **TaskStop the `swarm-discover` Monitor** so `ahs discover` never leaks.

## Notes

- The picker is not live: `AskUserQuestion` is a one-shot prompt, so the
  redraw is driven by the user picking `🔄 keep looking` — each refresh
  reopens it with whatever the Monitor has pushed since.
- The browse is read-only; you are not in a swarm until `/swarm:join` runs.
