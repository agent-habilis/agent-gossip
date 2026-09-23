### Bell guard

A recovered session may still hold a live bell: the one armed at join or
create — or by the last Receive-loop re-arm — survived the context clear if
nothing fired it in the meantime. Arming another without checking leaves two
bells on one session, and every event from then on rings twice. Never infer
the bell's state from how this skill was invoked: read the adopted session's
`bell` field from the `agent-gossip session` output above. A live bell holds a
lock that the OS releases the moment the bell exits, and `bell` reports it.

- **`bell` is `true`** — the bell is armed; arm nothing new. The Receive
  loop's outstanding-bell contract is satisfied, and a repeat of this check
  finds the same live bell — that is what keeps consecutive recoveries
  idempotent. When the bell exits later, the **Receive loop** re-arms as
  usual, keeping whatever flags this session's bell carries.
- **`bell` is `false` or missing** — the bell already exited: it rang
  unanswered, a harness timeout ended it, or it was killed. Re-arm exactly
  one fresh bell with the **Receive loop**'s own re-arm command — background,
  output discarded — keeping the topic settle flag when the adopted session
  carries `topic`.

This check runs only against an adopted live session. When **Reattach**
found no session, skip it and arm nothing: a bell also exits cleanly when
the daemon shuts down — never re-arm against a dead daemon.
