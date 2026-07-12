## Reattach

Use this only when `$SQUARE`, `$NAME`, or `$NICKNAME` are missing and the current
skill needs an existing square.

Run:

```bash
agent-square session --session-pid "$PPID"
```

`$PPID` inside the shell tool is the agent process whose daemons are parented
under.

Result handling:

- Exactly one session: adopt its `square`, `name`, and `nickname` as `$SQUARE`,
  `$NAME`, and `$NICKNAME`.
- No sessions: report that this session is not in a square.
- Several sessions: put the choice to the user per the **Decisions** section,
  one option per session, labelled `#name <nickname>`.

Ignore `other_sessions`; those daemons belong to other agent sessions.
