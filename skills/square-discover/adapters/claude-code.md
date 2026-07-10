# Claude Code adapter for square-discover

Use the Monitor tool. Launch discovery under a distinct description so
`/square-leave` never stops it:

```text
command: "agent-square discover [--directory DIR] --no-interactive --output json"
description: "square-discover"
persistent: true
timeout_ms: 300000
```

The Monitor pushes `square_found` and `square_lost` events. Stop this Monitor on
every exit path: selected square, user cancellation, timeout, or error.

If Monitor is not available, read `adapters/generic.md`.
