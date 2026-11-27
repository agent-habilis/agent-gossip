## Daemon session on Pi

Pi's own bash tool is synchronous and cannot host a background task; its
background facility is the `process` tool from the pi-processes extension,
whose exit notification is what wakes you. Before anything else, check your
tool list for `process`. If it is missing, print the block below and stop —
start nothing:

```text
💬 agent-gossip on pi needs the pi-processes extension
   install: pi install npm:@aliou/pi-processes
   then restart pi and run this skill again
```

With the tool present, issue the three calls sequentially (pi has no parallel
tool calls), mapped as:

- Tool call 1 → `process` action `start`, `name: "gossip-daemon"`, `command`
  exactly the daemon command of the **Daemon session** section. Keep the default
  alerts: a failure alert reports a daemon crash, and a clean daemon exit needs
  no alert.
- Tool call 2 → `process` action `start`, `name: "gossip-bell"`, `command`
  exactly the bell command of that section, plus `alertOnSuccess: true` — the
  bell exits cleanly by design, and only that alert wakes you. Set it on every
  re-arm.
- Tool call 3 → plain `bash`, unchanged.
