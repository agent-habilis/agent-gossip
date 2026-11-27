## Running commands in Antigravity

On Antigravity (agy), set `BypassSandbox` true on every `run_command` call that
runs `agent-gossip`. The Standard Sandbox cannot reach the daemon: a sandboxed
call fails with `No active gossip server running`. Prefer a command that starts
with `agent-gossip`: Always Allow matches the start of a command, and `&&`
chains, `$(id -u)` and `$VAR` can defeat the match.
