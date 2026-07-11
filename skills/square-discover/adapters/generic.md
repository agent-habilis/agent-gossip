## Generic adapter

Directory discovery has no poll API because it does not join a square. In a
shell-only harness, run a bounded foreground discovery and parse its JSON stdout:

```bash
agent-square discover [--directory DIR]
```

Stop the command after a short collection window if the harness supports command
timeouts. Use only complete JSON lines printed by the command; do not read logs.

If the harness cannot bound or interrupt a foreground command, print:

```text
💬 Discovery needs a cancellable foreground command or the Monitor tool.
Ask whoever runs the square for its `💬…` id and use
`${SKILL_PREFIX}square-join <id>` directly.
```
