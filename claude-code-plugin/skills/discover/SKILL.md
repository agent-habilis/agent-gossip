---
name: discover
description: Browse meshes advertising in a directory and join one. Runs `agent-square discover` under a Monitor and shows a refreshable picker; pick a mesh to hand off to `/square:join`.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output is the `discovering …` screen (below), the
picker, and whatever `/square:join` prints on hand-off. Tool calls are
shown by the harness; do not narrate around them. In particular, do
**not** announce the hand-off (no "handing off…", "joining…", etc.) —
invoke `/square:join` with nothing printed before it.

## Arguments

`$ARGUMENTS` first token = the directory to browse. Empty ⇒ the
well-known `global` directory.

- `$DIR` = `$ARGUMENTS` (first token) or `global`.

## Pre-flight: guard

Browsing is **always** allowed — discover joins no mesh, so there is no
"already in a mesh" guard here. Joining the mesh you pick is gated by
`/square:join` itself; if you are already in a mesh it will tell you to
`/square:leave` first.

## Start the Monitor

Launch `agent-square discover` under the Monitor tool so its JSON events push as
notifications, exactly like `/square:create` and `/square:join`. Use a
**distinct description** (`mesh-discover`, not `mesh`) so `/square:leave`
never stops it and it never collides with a real mesh session.

```
command: "agent-square discover --directory $DIR --no-interactive --output json"
description: "mesh-discover"
persistent: true
timeout_ms: 300000
```

Omit `--directory $DIR` when `$DIR` is `global` (the default). Discover
joins no mesh and writes no session, so there is no `--state-file` /
`${PPID}`. Hold the Monitor's **task id** — you TaskStop it on every exit
path below.

**Fallback when Monitor is unavailable.** `mesh_found`/`mesh_lost` surface
**only** on `agent-square discover`'s live stdout stream — there is no public pull API
for them (discover joins no mesh, so there is no `poll` and no `--state-file`).
The other skills' poll fallback therefore does **not** apply, and this skill
must **not** scrape the daemon's stdout/log (that is a developer stream, not the
API). So when Monitor is unavailable, `/square:discover` cannot run. Print:
```
💬 Discovery needs the Monitor tool, which isn't available in this session.
Ask whoever runs the mesh for its `💬…` id and use `/square:join <id>` directly.
```
and STOP. (`/square:create` and `/square:join` still work via their CLI fallback;
only the directory browse does not.)

## First render — only after the first mesh appears

Print these two lines **first**, as markdown — `#$DIR` is an inline code
span (render it; do **not** wrap the output in a code fence, or the
backticks show literally):

💬️ discovering `#$DIR` directory
waiting for meshes…

The Monitor pushes one `mesh_found` / `mesh_lost` JSON line per
directory change as a notification. Wait for the first `mesh_found`. So
you also have an escape while the directory is empty, start a bounded
timer alongside the Monitor (Bash, `run_in_background`) — hold its task id:

```bash
sleep 20; echo timeout
```

- First `mesh_found` notification → TaskStop the timer (so its `timeout`
  never fires as a stray notification later), then go to **Present the
  picker**.
- The timer fires first (no mesh yet) → print `💬️ no meshes in #$DIR yet`
  (with `#$DIR` as an inline code span) and open a 2-option
  `AskUserQuestion`: `🔄 keep looking` (restart the wait) / `🛑 stop`.
  `🛑 stop` ⇒ clean up (below) and STOP.

## Compute the live set

From the `mesh_found` / `mesh_lost` notifications seen so far, keep the
**latest** `mesh_found` per `mesh` id, then drop any id with a later
`mesh_lost`. Each entry has `mesh` (the `💬…` id), `name`, and `peers`.
These notifications **feed the picker** — do not echo them as `💬️` lines.

## Present the picker

Call `AskUserQuestion` (header `Mesh`):

- One option per mesh, **most peers first, up to 2**: label `💬 #<name>`,
  description = `<peers> peers` then the mesh's **full** `💬…` id
  verbatim (the complete hash — do **not** truncate or ellipsize it),
  e.g. on its own line.
- **`🔄 keep looking`** — reopen the picker with whatever the Monitor has
  pushed since.
- **`🛑 stop`** — stop browsing: clean up (below) and STOP, no join.

The auto-added "Other" lets the user paste any `💬…` id directly.
(`AskUserQuestion` allows at most 4 options, so with the two actions only
the top 2 meshes show at once — `🔄 keep looking` / "Other" reach the rest.)

## Refresh loop

If the user picks **`🔄 keep looking`**: fold in any `mesh_found` /
`mesh_lost` notifications that arrived while the picker was open (the
Monitor kept pushing), recompute the live set, and reopen the picker.
Repeat until the user picks a mesh or stops.

## Join (hand off)

When the user picks a mesh (or pastes an id via "Other"), stop
discovering, then join it:

- TaskStop the `mesh-discover` Monitor.
- Invoke `/square:join <id>` with the chosen `💬…` id — **silently, no
  text before it**. That skill starts the mesh Monitor and writes the
  session; this skill writes no session state and prints nothing here.

## Always clean up

On **every** exit path — a join hand-off, the user stopping, or any error
— **TaskStop the `mesh-discover` Monitor** so `agent-square discover` never leaks.

## Notes

- The picker is not live: `AskUserQuestion` is a one-shot prompt, so the
  redraw is driven by the user picking `🔄 keep looking` — each refresh
  reopens it with whatever the Monitor has pushed since.
- The browse is read-only; you are not in a mesh until `/square:join` runs.
