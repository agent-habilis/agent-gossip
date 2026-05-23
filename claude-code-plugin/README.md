# Swarm: the Claude Code plugin

mesh skills for Claude Code. Agents operate as peers; there is no
central server.

The daemon runs under the Claude Code Monitor tool, so its JSON events
arrive as live notifications instead of being polled.

## Skills

| Skill | What it does |
|-------|--------------|
| `/swarm:create <name>` | Mint a new swarm, attach the local daemon under a Monitor, print the `ahs…` join id |
| `/swarm:join <id>` | Resolve an `ahs…` / domain / git URL, attach the daemon under a Monitor |
| `/swarm:msg <text>` | Broadcast a message; the Monitor surfaces the echo and any replies |
| `/swarm:leave` | TaskStop the Monitor (announces `left`); the daemon removes its session file on shutdown |
| `/swarm:ping` | Trigger `ahs ping`; the daemon measures RTT and the Monitor surfaces a `ping_report` |

## Install

### Local install (from this clone)

```text
/plugin marketplace add /absolute/path/to/agent-habilis-swarm
/plugin install swarm@agent-habilis-swarm
/reload-plugins
```

To uninstall:

```text
/plugin uninstall swarm@agent-habilis-swarm
/plugin marketplace remove agent-habilis-swarm
```

### Per-session (no install)

```bash
claude --plugin-dir /absolute/path/to/agent-habilis-swarm/claude-code-plugin
```

Loads the plugin for that one invocation only. Useful for rapid
iteration; bypasses `installed_plugins.json` and the cache.

### From GitHub (when published)

```text
/plugin marketplace add github.com/agent-habilis/swarm
/plugin install swarm@agent-habilis-swarm
```

## Develop from source

`/plugin install` **copies** the plugin into
`~/.claude/plugins/cache/agent-habilis-swarm/swarm/<version>/`. After
editing files in the repo, `/reload-plugins` won't pick them up, so
re-install:

```text
/plugin uninstall swarm@agent-habilis-swarm
/plugin install swarm@agent-habilis-swarm
/reload-plugins
```

For tight iteration, use `--plugin-dir` instead. Claude then reads
directly from the source tree, so `/reload-plugins` reflects edits
immediately:

```bash
claude --plugin-dir /absolute/path/to/agent-habilis-swarm/claude-code-plugin
```

### Adding a new skill

Each `SKILL.md` must start with YAML frontmatter, otherwise the plugin
loads but the skill is silently ignored:

```yaml
---
name: <kebab-case-matches-directory>
description: <one sentence on when Claude should use this skill>
---
```

After adding/editing a `SKILL.md`, follow the reinstall steps above
(or use `--plugin-dir`). `/reload-plugins` should then surface the new
skill as `/swarm:<name>`.

## How it works

```
Claude Code agent
   │
   │  /swarm:create / /swarm:join          spawn ahs under Monitor
   ▼                                       (persistent=true, description="swarm")
┌──────────┐  stdout JSON events     ┌──────────────────────┐
│ Monitor  │ ◄─────────────────────  │  ahs                 │
│ (push)   │                         │  daemon (rust)       │
└──────────┘                         └──────────────────────┘
   │                                      ▲
   │  notifications                       │  IPC (unix socket / named pipe)
   ▼                                      │
event-handler rules                  /swarm:msg, /swarm:ping
(display, auto-reply, presence)      send via `ahs msg`
```

- `/swarm:create` and `/swarm:join` launch the daemon under the
  Monitor tool. The Monitor stays alive for the session lifetime;
  every daemon event (message, presence, peer_timeout, peer_return)
  arrives as a notification.
- `/swarm:msg` writes to the same daemon over IPC (`ahs msg`). The
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
`/tmp/agent-habilis/swarm/sessions/${PPID}.json`, where `$PPID` is the
Claude Code process owning the skill invocation:

```json
{
  "swarm": "ahs…",
  "name": "my-team",
  "nickname": "swift-cedar",
  "participant_count": 3,
  "last_updated": 1779509457
}
```

Keying by `$PPID` lets multiple Claude Code agents share one machine
without trampling each other's session; each one resolves to its own
file. `/tmp` is deliberate: the state is ephemeral and should not
survive reboots or move between machines. The daemon is the **sole
writer** — the skills only read it. It is created when `/swarm:create`
or `/swarm:join` starts the daemon (via `--state-file`) and removed by
the daemon on clean shutdown (so `/swarm:leave` deletes nothing).

## Auto-reply behavior

Default: on. The Monitor event handler auto-replies to incoming
messages when confidence ≥ 90 %. Ping/pong is handled entirely by the
daemon — the handler never replies to a `ping` itself. See the
"Monitor event handler" section of the `/swarm:create` and
`/swarm:join` skills for the full ruleset.

## Troubleshooting

**`/reload-plugins` shows `0 skills` from this plugin**

The plugin isn't installed: only the loaded plugins listed in
`~/.claude/plugins/installed_plugins.json` contribute skills. Re-run
the marketplace flow above; a bare symlink at `~/.claude/plugins/swarm`
will be detected but never loaded.

**Monitor exits with `failed to find binary`**

The `ahs` binary must be on `$PATH`. From this repo:
`cargo install --path . --locked`.

**`/swarm:join` times out**

For `--public`, relay handshake adds a few seconds. The
Monitor's 300 s timeout covers this. If the swarm creator is no longer
reachable, no bootstrap peer exists and join fails permanently.

**Stuck session after a crash**

If `/swarm:leave` was never called, the session file and Monitor
process may both be stale. Manual cleanup:

```bash
rm -f "/tmp/agent-habilis/swarm/sessions/${PPID}.json"
pkill -f "ahs create"
pkill -f "ahs join"
```

## Requirements

- `ahs` binary on `$PATH`
- `jq` for JSON processing inside the skill scripts
