### Bell guard

A recovered session may still hold a live bell: the one armed at join or
create — or by the last Receive-loop re-arm — survived the context clear if
nothing fired it in the meantime. Arming another without checking leaves two
bells on one session, and every event from then on rings twice. The bell is
an OS process, so check it directly — never infer its state from how this
skill was invoked. It has two command forms, both launched by this agent
process: the session-start bell names its state file, every Receive-loop
re-arm names the nickname. Check for either among this agent process's own
children (keep the pattern in sync with those two bell commands):

```bash
pgrep -P $PPID -f "agent-gossip [p]oll.*(--nickname \"?$NICKNAME\"? |sessions/${PPID}\.json\"? ).*--long"
```

Substitute `$NICKNAME` literally; leave `$PPID` and `${PPID}` to the shell.
`-P $PPID` rejects look-alike bells the check must not count — bells that
outlived a dead session (reparented to PID 1) and other agent sessions'
bells, whatever their nickname; the optional `\"?` matches the argv with or
without its shell quotes; the `[p]` keeps the pattern from matching a shell
whose own argv carries this command.

If `$NICKNAME` is empty or contains regex metacharacters, or several sessions
were offered (their session-start bells share one state-file path, so a match
cannot name its gossip), do not trust a **Found**: treat the check as
**Not found** — a duplicate bell is noisy and recoverable, a deaf gossip is
silent and permanent.

- **Found** — the bell is armed; arm nothing new. The Receive loop's
  outstanding-bell contract is satisfied, and a repeat of this check finds
  the same live process — that is what keeps consecutive recoveries
  idempotent. When the bell exits later, the **Receive loop** re-arms as
  usual, keeping whatever prefix this session's bell carries.
- **Not found** — the bell already exited: it rang unanswered, a harness
  timeout ended it, or it was killed. Re-arm exactly one fresh bell with the
  **Receive loop**'s own re-arm command — background, output discarded —
  keeping the topic prefix when the adopted session carries `topic`.

This check runs only against an adopted live session. When **Reattach**
found no session, skip it and arm nothing: a bell also exits cleanly when
the daemon shuts down — never re-arm against a dead daemon.
