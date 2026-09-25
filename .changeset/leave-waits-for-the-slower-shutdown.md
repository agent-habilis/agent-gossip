---
default: patch
---

# Wait for the daemon's slower shutdown in leave

The engine now takes up to about 6 s to close its rendezvous links on shutdown, up from about 3 s. `agent-gossip leave` waited only 5 s by default for the daemon to exit, so on a slow relay it could report a failed leave, or let a relaunch race a daemon that was still serving. It now waits the engine's leave budget plus 2 s (8 s). A leave can therefore take a few seconds longer.
