---
default: minor
---

# Run on Windows

agent-gossip builds and runs on Windows (x86_64-pc-windows-msvc), and a release ships a `.zip` for it. With `--owner-pid` the daemon starts with no console, in its own process group and outside the job of its launcher, and it holds no pipe of the caller. `leave` stops a daemon with the IPC quit of the engine. On Windows `--owner-pid`, `--session-pid`, `--state-file` and the new `--stderr-file` take `auto`: the binary finds the agent process, the first ancestor that is not a shell, because `$PPID` is 1 under Git Bash. The skills for Windows use `auto` and run the Stop hook in exec form. Linux and macOS do not change.
