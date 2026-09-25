---
default: patch
---

# Stop a full disk from aborting the daemon through its log

When a log write failed on a full disk, the log library reported the failure to stderr, which was a file on the same full disk. That report failed too and aborted the daemon with exit 134, an empty stderr, and no farewell to its peers; the shutdown path hit it every time. A failed log write is now dropped instead. The skills also keep the previous `.stderr` file as `.stderr.prev` when they relaunch a daemon, so the last daemon's errors are no longer erased.

Still open, in the engine: the state-file heartbeat reports its own write failure the same way, so a daemon on a full disk can still abort within about 10 s when its state file is on that disk. A short command that flushes a buffered log to stderr at exit (for example `poll`) can abort the same way.
