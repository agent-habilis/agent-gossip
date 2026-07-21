## Generic adapter

Directory discovery has no poll API because it does not join a gossip. Run:

```bash
agent-gossip discover [--directory DIR] --window-secs 25
```

The command exits on its own when the window closes — no harness timeout or
kill needed. If the harness can run it in the background and poll its interim
output, poll every few seconds and apply the **Selection** rule as soon as the
first `gossip_found` appears; otherwise run it in the foreground and read its
output when it exits. Use only complete JSON lines printed by the command; do
not read logs.
