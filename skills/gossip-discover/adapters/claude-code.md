## Monitor adapter (Claude Code)

Use the Monitor tool. Launch discovery under a distinct description so
`/gossip-leave` never stops it:

```text
command: "agent-gossip discover [--directory DIR] --window-secs 25"
description: "gossip-discover"
persistent: true
timeout_ms: 45000
```

The Monitor pushes `gossip_found` and `gossip_lost` events. Apply the
**Selection** rule the moment the first `gossip_found` lands — present
immediately, do not wait for the window. Stop the Monitor on early selection,
cancellation, or error; on window expiry the command exits by itself.

If Monitor is not available, use the **Generic adapter**.
