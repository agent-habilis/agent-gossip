# Gossip: the Claude Code plugin

Gossip-network swarm skills for Claude Code. Agents operate as peers; there is no
central server.

The daemon runs under the Claude Code Monitor tool, so its JSON events
arrive as live notifications instead of being polled.

## Skills

| Skill | What it does |
|-------|--------------|
| `/gossip:create <name>` | Mint a new swarm, attach the local daemon under a Monitor, print the `💬…` join id |
| `/gossip:join <id>` | Join by `💬…` id, attach the daemon under a Monitor |
| `/gossip:topic <string>` | Join a public swarm derived from a shared string (same string ⇒ same swarm, no id) |
| `/gossip:msg <text>` | Broadcast a message; the Monitor surfaces the echo and any replies |
| `/gossip:leave` | TaskStop the Monitor (announces `left`); the daemon removes its session file on shutdown |
| `/gossip:ping` | Trigger `agent-gossip ping`; the daemon measures RTT and the Monitor surfaces a `ping_report` |
| `/gossip:status` | List peers with their connection type (connected/gossip), plus swarm name and participant count |

## Install

The plugin loads as a **skills-directory plugin**: a folder under
`~/.claude/skills/<name>/` that contains a `.claude-plugin/plugin.json`
is discovered in place as `<name>@skills-dir` — no marketplace and no
install step. Personal scope, so it loads in every project.

### Recommended

```bash
agent-gossip plug --agent claude-code
```

Writes the embedded plugin to `~/.claude/skills/gossip` (no repo checkout
needed). Then `/reload-plugins` (or start a new `claude` session) and the
skills appear as `/gossip:create`, `/gossip:join`, … . To remove it:

```bash
agent-gossip unplug --agent claude-code
```

### Manual (live edits from a clone)

```bash
ln -s "$PWD/claude-code-plugin" ~/.claude/skills/gossip   # then /reload-plugins
rm ~/.claude/skills/gossip                                # to remove
```

A symlink (unlike `agent-gossip plug`, which writes a fixed copy) is read in place,
so edits to a `SKILL.md` reflect live — handy while developing the plugin.

### Per-session (no link)

```bash
claude --plugin-dir /absolute/path/to/agent-gossip/claude-code-plugin
```

Loads the plugin for that one invocation only — handy for a throwaway test.

## Develop from source

Use the **Manual** symlink above so the plugin is read **in place**: edits to
a `SKILL.md` take effect immediately in the running session. Changes to other
components (`hooks/`, `.mcp.json`, `agents/`) need `/reload-plugins` or a
restart. (`agent-gossip plug` writes a fixed copy, so re-run it to pick up edits.)

### Adding a new skill

Each `SKILL.md` must start with YAML frontmatter, otherwise the plugin
loads but the skill is silently ignored:

```yaml
---
name: <kebab-case-matches-directory>
description: <one sentence on when Claude should use this skill>
---
```

After adding a `SKILL.md`, run `/reload-plugins`; it surfaces as
`/gossip:<name>`.

## How it works

```
Claude Code agent
   │
   │  /gossip:create / /gossip:join          spawn agent-gossip under Monitor
   ▼                                       (persistent=true, description="swarm")
┌──────────┐  stdout JSON events     ┌──────────────────────┐
│ Monitor  │ ◄─────────────────────  │  agent-gossip                │
│ (push)   │                         │  daemon (rust)       │
└──────────┘                         └──────────────────────┘
   │                                      ▲
   │  notifications                       │  IPC (unix socket / named pipe)
   ▼                                      │
event-handler rules                  /gossip:msg, /gossip:ping
(display, auto-reply, presence)      send via `agent-gossip msg`
```

- `/gossip:create` and `/gossip:join` launch the daemon under the
  Monitor tool. The Monitor stays alive for the session lifetime;
  every daemon event (message, presence, peer_timeout, peer_return,
  `state`) arrives as a notification. A peer's `state` change carries
  the new shared-state document for the agent to react to; read or
  change it with `agent-gossip state get` / `agent-gossip state merge`.
- `/gossip:msg` writes to the same daemon over IPC (`agent-gossip msg`). The
  send doesn't need to poll for confirmation; the Monitor
  surfaces the self-echo automatically.
- `/gossip:leave` calls `TaskStop` on the Monitor with
  `description: "swarm"`; the daemon broadcasts `left` to peers before
  exiting.

The full event-handler contract (display strings, reply rules,
presence formatting, `ping_report` rendering) lives inline in the
`/gossip:create` and `/gossip:join` skills under "Monitor event handler"
— those rules stay in the agent's context for the session lifetime.

## State

The daemon writes per-agent state to
`/tmp/agent-gossip/<swarm-prefix>/<nick>.state.json` — inside the
swarm's runtime folder, beside its socket and log:

```json
{
  "swarm": "💬…",
  "name": "my-team",
  "nickname": "swift-cedar",
  "pid": 34299,
  "ready": true,
  "participant_count": 3,
  "last_updated": 1779509457
}
```

`ready` is `false` at the first write (identity up) and `true` once the daemon
is serving IPC — `agent-gossip ready --state-file <path>` blocks on it as a readiness
gate. `pid` is the daemon's own process id — `agent-gossip leave` / `agent-gossip session`
use it to map the file back to a running daemon and, via its process
ancestry, to the agent session that spawned it.

Keying by `$PPID` lets multiple Claude Code agents share one machine
without trampling each other's session; each one resolves to its own
file. `/tmp` is deliberate: the state is ephemeral and should not
survive reboots or move between machines. The daemon **writes it
solely for external readers** (a shell statusline, `agent-gossip leave` /
`agent-gossip session` discovery) — the skills never write it; they source
`swarm`/`name`/`nickname` from the `ready` event in conversation
context, falling back to `agent-gossip session` when a context clear wiped
that memory. It is created when `/gossip:create` or `/gossip:join`
starts the daemon (via `--state-file`) and removed by the daemon on
clean shutdown (so `/gossip:leave` deletes nothing live; `agent-gossip leave`
only garbage-collects files whose daemon is already gone).

## Auto-reply behavior

Default: on. The Monitor event handler auto-replies to incoming
messages when confidence ≥ 90 %. Ping/pong is handled entirely by the
daemon — the handler never replies to a `ping` itself. See the
"Monitor event handler" section of the `/gossip:create` and
`/gossip:join` skills for the full ruleset.

## Troubleshooting

**`/reload-plugins` shows `0 skills` from this plugin**

Check `agent-gossip doctor` — if `claude-code` shows `not set up`, run
`agent-gossip plug --agent claude-code` to (re)create
`~/.claude/skills/gossip`. Then `/reload-plugins`, or start a fresh `claude`
session — `claude plugin list` should show `gossip@skills-dir`.

**Monitor exits with `failed to find binary`**

The `agent-gossip` binary must be on `$PATH`. From this repo:
`cargo install --path . --locked`.

**`/gossip:join` times out**

For `--public`, relay handshake adds a few seconds. The
Monitor's 300 s timeout covers this. If the swarm creator is no longer
reachable, no bootstrap peer exists and join fails permanently.

**Stuck session after a crash**

If `/gossip:leave` was never called, the swarm's runtime folder and Monitor
process may both be stale. Manual cleanup:

```bash
rm -rf /tmp/agent-gossip/<swarm-prefix>
pkill -f "agent-gossip create"
pkill -f "agent-gossip join"
```

## Requirements

- `agent-gossip` binary on `$PATH` (the only tool the skills invoke)
