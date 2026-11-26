---
default: minor
---

# Add the Antigravity CLI (agy) to plug

`agent-gossip plug` now installs an `agent-gossip` plugin into `~/.gemini/config/plugins` when it finds agy. The plugin carries the skills and a Stop hook that refuses a turn that ends with no bell armed. `bell-check` takes `--protocol antigravity` to answer in the agy hook protocol.
