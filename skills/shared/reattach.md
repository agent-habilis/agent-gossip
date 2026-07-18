## Reattach

Use this only when `$GOSSIP`, `$NAME`, or `$NICKNAME` are missing and the current
skill needs an existing gossip.

Run:

```bash
agent-gossip session --session-pid "$PPID"
```

`$PPID` inside the shell tool is the agent process whose daemons are parented
under.

Result handling:

- Exactly one session: adopt its `gossip`, `name`, and `nickname` as `$GOSSIP`,
  `$NAME`, and `$NICKNAME`.
- No sessions: report that this session is not in a gossip.
- Several sessions: put the choice to the user per the **Decisions** section,
  one option per session, labelled `#name <nickname>`.

Ignore `other_sessions`; those daemons belong to other agent sessions.
