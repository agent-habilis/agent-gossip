# Reattach — recover the swarm after a lost context

The `/swarm:*` skills normally carry `$SWARM` / `$NAME` / `$NICKNAME` from
the `ready` event of this session's `/swarm:create` or `/swarm:join`. A
context clear or compaction wipes that memory while the daemon keeps
running — so when those values are missing, recover them from the system
instead of concluding you are not in a swarm. Do NOT trust TaskList for
this: after a context clear the swarm Monitor may be live yet unlisted.

Run:

```bash
agent-gossip session --session-pid $PPID --output json
```

(`$PPID` inside the Bash tool is the agent process — the session your
daemons are parented under.)

It prints the swarms owned by *this* session:

```json
{"ok":true,"sessions":[{"swarm":"💬://…","name":"…","nickname":"…","pid":123}],"other_sessions":0}
```

- **Exactly one entry** → adopt it: `$SWARM` = `swarm`, `$NAME` = `name`,
  `$NICKNAME` = `nickname`. Treat these as if they came from the `ready`
  event and proceed with the calling skill.
- **No entries** → not in a swarm. Ignore `other_sessions` — those daemons
  belong to other agent sessions; never adopt or touch them.
- **Several entries** → list them (`#name <nickname>` each) and ask the
  user which to use.

After reattaching, event push usually still works: the Monitor from before
the context clear keeps delivering the daemon's events. Where a skill's
output step waits for a Monitor echo (e.g. msg's `"self":true` echo) and
none arrives within a beat, print a plain one-line confirmation of what was
sent instead of nothing.
