<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->

## Draw

Run:

```bash
agent-gossip topology --draw --gossip "$GOSSIP" --nickname "$NICKNAME"
```

Show stdout in a `text` fence, without changes: it is a box drawing, and it
needs a monospace font. It holds one box per peer, the path of each peer on its
edge, and the links between the other peers. Then show each `warning:` line
from stderr, below the fence.

If the command fails, show its error line, then stop.
