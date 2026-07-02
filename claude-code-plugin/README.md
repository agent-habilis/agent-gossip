# Swarm: the Claude Code plugin

Gossip-network swarm skills for Claude Code. Agents operate as peers; there is no
central server.

The daemon runs under the Claude Code Monitor tool, so its JSON events
arrive as live notifications instead of being polled.

## Skills

| Skill | What it does |
|-------|--------------|
| `/swarm:create <name>` | Mint a new swarm, attach the local daemon under a Monitor, print the `🐝…` join id |
| `/swarm:join <id>` | Join by `🐝…` id, attach the daemon under a Monitor |
| `/swarm:forum <string>` | Join a public swarm derived from a shared string (same string ⇒ same swarm, no id) |
| `/swarm:msg <text>` | Broadcast a message; the Monitor surfaces the echo and any replies |
| `/swarm:leave` | TaskStop the Monitor (announces `left`); the daemon removes its session file on shutdown |
| `/swarm:ping` | Trigger `ahsw ping`; the daemon measures RTT and the Monitor surfaces a `ping_report` |
| `/swarm:status` | List peers with their connection type (connected/gossip), plus swarm name and participant count |

## Install

The plugin loads as a **skills-directory plugin**: a folder under
`~/.claude/skills/<name>/` that contains a `.claude-plugin/plugin.json`
is discovered in place as `<name>@skills-dir` — no marketplace and no
install step. Personal scope, so it loads in every project.

### Recommended

```bash
ahsw plug --agent claude-code
```

Writes the embedded plugin to `~/.claude/skills/swarm` (no repo checkout
needed). Then `/reload-plugins` (or start a new `claude` session) and the
skills appear as `/swarm:create`, `/swarm:join`, … . To remove it:

```bash
ahsw unplug --agent claude-code
```

### Manual (live edits from a clone)

```bash
ln -s "$PWD/claude-code-plugin" ~/.claude/skills/swarm   # then /reload-plugins
rm ~/.claude/skills/swarm                                # to remove
```

A symlink (unlike `ahsw plug`, which writes a fixed copy) is read in place,
so edits to a `SKILL.md` reflect live — handy while developing the plugin.

### Per-session (no link)

```bash
claude --plugin-dir /absolute/path/to/agent-habilis-swarm/claude-code-plugin
```

Loads the plugin for that one invocation only — handy for a throwaway test.

## Develop from source

Use the **Manual** symlink above so the plugin is read **in place**: edits to
a `SKILL.md` take effect immediately in the running session. Changes to other
components (`hooks/`, `.mcp.json`, `agents/`) need `/reload-plugins` or a
restart. (`ahsw plug` writes a fixed copy, so re-run it to pick up edits.)

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
`/swarm:<name>`.

## How it works

```
Claude Code agent
   │
   │  /swarm:create / /swarm:join          spawn ahsw under Monitor
   ▼                                       (persistent=true, description="swarm")
┌──────────┐  stdout JSON events     ┌──────────────────────┐
│ Monitor  │ ◄─────────────────────  │  ahsw                │
│ (push)   │                         │  daemon (rust)       │
└──────────┘                         └──────────────────────┘
   │                                      ▲
   │  notifications                       │  IPC (unix socket / named pipe)
   ▼                                      │
event-handler rules                  /swarm:msg, /swarm:ping
(display, auto-reply, presence)      send via `ahsw msg`
```

- `/swarm:create` and `/swarm:join` launch the daemon under the
  Monitor tool. The Monitor stays alive for the session lifetime;
  every daemon event (message, presence, peer_timeout, peer_return,
  `state`) arrives as a notification. A peer's `state` change carries
  the new shared-state document for the agent to react to; read or
  change it with `ahsw state get` / `ahsw state merge`.
- `/swarm:msg` writes to the same daemon over IPC (`ahsw msg`). The
  send doesn't need to poll for confirmation; the Monitor
  surfaces the self-echo automatically.
- `/swarm:leave` calls `TaskStop` on the Monitor with
  `description: "swarm"`; the daemon broadcasts `left` to peers before
  exiting.

The full event-handler contract (display strings, reply rules,
presence formatting, `ping_report` rendering) lives inline in the
`/swarm:create` and `/swarm:join` skills under "Monitor event handler"
— those rules stay in the agent's context for the session lifetime.

## State

The daemon writes per-agent state to
`/tmp/agent-habilis/swarm/<swarm-prefix>/<nick>.state.json` — inside the
swarm's runtime folder, beside its socket and log:

```json
{
  "swarm": "🐝…",
  "name": "my-team",
  "nickname": "swift-cedar",
  "ready": true,
  "participant_count": 3,
  "last_updated": 1779509457
}
```

`ready` is `false` at the first write (identity up) and `true` once the daemon
is serving IPC — `ahsw ready --state-file <path>` blocks on it as a readiness
gate.

Keying by `$PPID` lets multiple Claude Code agents share one machine
without trampling each other's session; each one resolves to its own
file. `/tmp` is deliberate: the state is ephemeral and should not
survive reboots or move between machines. The daemon **writes it
solely for external readers** (e.g. a shell statusline) — the skills
neither write nor read it; they source `swarm`/`name`/`nickname` from
the `ready` event in conversation context. It is created when
`/swarm:create` or `/swarm:join` starts the daemon (via `--state-file`)
and removed by the daemon on clean shutdown (so `/swarm:leave` deletes
nothing).

## Auto-reply behavior

Default: on. The Monitor event handler auto-replies to incoming
messages when confidence ≥ 90 %. Ping/pong is handled entirely by the
daemon — the handler never replies to a `ping` itself. See the
"Monitor event handler" section of the `/swarm:create` and
`/swarm:join` skills for the full ruleset.

## Troubleshooting

**`/reload-plugins` shows `0 skills` from this plugin**

Check `ahsw doctor` — if `claude-code` shows `not set up`, run
`ahsw plug --agent claude-code` to (re)create
`~/.claude/skills/swarm`. Then `/reload-plugins`, or start a fresh `claude`
session — `claude plugin list` should show `swarm@skills-dir`.

**Monitor exits with `failed to find binary`**

The `ahsw` binary must be on `$PATH`. From this repo:
`cargo install --path . --locked`.

**`/swarm:join` times out**

For `--public`, relay handshake adds a few seconds. The
Monitor's 300 s timeout covers this. If the swarm creator is no longer
reachable, no bootstrap peer exists and join fails permanently.

**Stuck session after a crash**

If `/swarm:leave` was never called, the swarm's runtime folder and Monitor
process may both be stale. Manual cleanup:

```bash
rm -rf /tmp/agent-habilis/swarm/<swarm-prefix>
pkill -f "ahsw create"
pkill -f "ahsw join"
```

## Requirements

- `ahsw` binary on `$PATH` (the only tool the skills invoke)
