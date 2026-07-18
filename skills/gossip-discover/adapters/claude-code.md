## Monitor adapter (Claude Code)

Use the Monitor tool. Launch discovery under a distinct description so
`/gossip-leave` never stops it:

```text
command: "agent-gossip discover [--directory DIR]"
description: "gossip-discover"
persistent: true
timeout_ms: 300000
```

The Monitor pushes `gossip_found` and `gossip_lost` events. Stop this Monitor on
every exit path: selected gossip, user cancellation, timeout, or error.

If Monitor is not available, use the **Generic adapter**.
