---
name: create
description: Create a new swarm and attach the local daemon under a Monitor. Use when the user wants to start a new swarm session with a fresh `ahs…` join id.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just
did. The only text output for the whole skill is the final
confirmation block under "Output". Bash tool calls (and any
Monitor invocation) are allowed — the harness shows them; just
do not narrate around them.

## Pre-flight: guard

**Already in a swarm?** Judge this from **conversation context only** —
if you ran `/swarm:create` or `/swarm:join` earlier in this session and
have not since run `/swarm:leave`, do NOT create another. Print:
```
Already in a swarm. Use /swarm:leave first if you want to create a new one.
```
and STOP.

## Resolve the swarm name

`ahs create` takes an **optional** `--name {NAME}`. When given, the name is
1-32 UTF-8 characters (any script/emoji), excluding control characters,
whitespace, and any of `/ \ < > #`. It is bound cryptographically into the
swarm identity — joiners decode it from the swarm ID, and a forged name will
not find peers. When omitted, the daemon mints a random `word-word` name (the
same style as a nickname).

If the user passed a name as an argument to the skill, use it — the CLI is the
final validator, so pass it through and let `ahs` reject a bad one. Otherwise
do **not** prompt: omit `--name` entirely and let the daemon mint a random
name. Never pass an empty `--name ""` (the CLI rejects it). The actual name
comes back in the `ready` event either way.

## Start the Monitor

Launch the daemon under the Monitor tool so its JSON events push as
notifications instead of needing to be polled:

```
command: "ahs create [--name {NAME}] --state-file /tmp/agent-habilis/swarm/sessions/${PPID}.json --no-interactive --output json"
description: "swarm"
persistent: true
timeout_ms: 300000
```

Include `--name {NAME}` only when the user supplied a name; omit the flag
entirely otherwise (do not pass an empty value).

The Monitor runs the command in the same shell environment as Bash, so
`${PPID}` expands to the parent Claude Code process — the same per-agent
key the sibling skills (`msg`, `leave`, …) use to find this file. Type
`${PPID}` verbatim into the command; do not substitute it yourself.

Add `--public` if the user requests cross-network connectivity (e.g.
connecting from different machines or networks). Add `--relay {URL}`
together with `--public` to pin a custom relay.

Add `--advertise[={DIRECTORY}]` when the user wants the swarm listed in a
directory so others can find it with `ahs discover` (no id to share) — it
requires the public network, so add `--public` too. Bare `--advertise` ⇒ the
well-known `global` directory; `--advertise {DIRECTORY}` ⇒ a named one. When
you add it, hold the directory name as `$DIRECTORY` (the value you passed, or
`global` when bare) for the Output below; otherwise leave `$DIRECTORY` unset.

## Parse the ready event

The first event from the Monitor will be:
```
{"event":"ready","swarm":"ahs...","name":"...","nickname":"..."}
```

From this event, hold three values for the rest of the skill:

- `$SWARM`    = `ready.swarm`    (the `ahs...` id)
- `$NAME`     = `ready.name`     (the swarm name)
- `$NICKNAME` = `ready.nickname` (your assigned `word-word` nick)

All three are required. If any is missing/empty, or if the Monitor
exits before the ready event arrives, print `failed to create swarm`
and STOP.

The self-presence `joined` event arriving in the same Monitor batch is
redundant with the output below — skip it.

The daemon persists `swarm`, `name`, and `nickname` to the
`--state-file` path, so this skill writes nothing — it is read-only.
Sibling skills (`msg`, `reply`, `leave`, `ping`) read those
keys from there.

## Output

Print (include the `advertising` line **only** when you added `--advertise`;
`$DIRECTORY` is the directory you advertised into, `global` if bare):
```
🐝️ created `#$NAME` and joined as `<$NICKNAME>`
advertising on `#$DIRECTORY`
others can join with: `/swarm:join $SWARM`
```
Omit the `advertising` line entirely when not advertising.

## Notes

- The Monitor holds the daemon for the session lifetime. Use
  `/swarm:leave` to TaskStop it cleanly.
- Swarm IDs encode network mode AND the swarm name, so the join hint is
  always: `/swarm:join {ahs...}`

## Monitor event handler (after create exits)

The Monitor pushes JSON events as `task-notification` messages for the rest
of the session. These arrive *after* this skill returns, so the rules below
must stay in your context.

**CRITICAL: every event carries a pre-built `display` string. Emit that
value VERBATIM — nothing added, nothing changed.** The daemon builds
`display` as the single source of truth for what the user sees: the `🐝️`
prefix, the literal backticks around nicks (a code span, so the terminal
markdown renderer does not eat `<nick>` as an HTML tag), the `→` arrow, and
the message **body byte-for-byte**. NEVER compose the line yourself from the
`author`/`body`/`reply` fields. NEVER reword, re-case, re-space, trim,
translate, summarize, paraphrase, or wrap it in prose. NEVER batch events
into a digest, or add a preamble/postamble. One event in → its `display`
value out, or silence.

**Skip silently** (zero output, no narration, no log):

- `event` is `info`, `error`, `msg_posted`, `ready`, or `fork`
- `type` is `presence` with `"subtype":"alive"`
- a `presence` message (`"type":"presence"`) with `"self":true` — your own
  join/leave is already covered by this skill's Output / `/swarm:leave`.

**Show your own `msg` events.** A `msg` event with `"self":true` is your
outbound message (sent via `/swarm:msg` or `/swarm:reply`) echoed back by
the daemon — emit its `display` verbatim. That echo IS the outbound
confirmation; never also re-render the text elsewhere.

**Everything else carries `display`** — `msg` (yours or a peer's),
`presence` joined/left, `peer_timeout`, `peer_return`, and `ping_report`.
Print the event's `display` field verbatim. For `ping_report` the `display`
field is the full multi-line RTT table — emit it exactly as given.

Arrival/departure surface exactly once each, as `presence joined` /
`presence left`. There is no transport-level `peer_join`/`peer_leave` to
de-duplicate against anymore.

**Replies**

- Reply only when you are >=90% confident; address with `--reply
  <author>`. A wrong reply is worse than silence. Replies are plain
  messages addressed to a nickname via `--reply`, not threaded by
  parent id.
- **Ping/pong is handled entirely by the daemon** — do NOT reply to a
  `ping` message yourself; the daemon auto-pongs and produces the
  `ping_report`.
