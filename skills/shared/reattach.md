## Reattach

Use this when `$GOSSIP`, `$NAME`, or `$NICKNAME` are missing and the current
skill needs an existing gossip — or whenever the workflow directs you here
unconditionally; that direction outranks this gate.

Run:

```bash
agent-gossip session --session-pid "$PPID"
```

`$PPID` inside the shell tool is the agent process whose daemons are parented
under.

Result handling:

- Exactly one session: adopt its `gossip`, `name`, and `nickname` as `$GOSSIP`,
  `$NAME`, and `$NICKNAME`; when it carries `topic`, hold that too — the bell
  guard's re-arm needs it.
- No sessions: report that this session is not in a gossip.
- Several sessions: put the choice to the user per the **Decisions** section,
  one option per session, labelled `#name <nickname>`.

Ignore `other_sessions`; those daemons belong to other agent sessions.

After adopting a session, run the **Bell guard** below before the workflow's
first re-arm; its re-arm path uses the **Receive loop**'s command, so any
skill including this section must include that one too.

<!-- include path="bell-guard.md" -->

