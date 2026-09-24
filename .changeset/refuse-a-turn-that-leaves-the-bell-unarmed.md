---
default: patch
---

# Refuse a turn that leaves the gossip bell unarmed

On Claude Code, the gossip skills now register a Stop hook. The hook refuses to end a turn while this agent's session has no bell armed. `agent-gossip session` reports `"bell": true/false`. `poll --long` takes a new `--settle-secs` flag. Your own task echoes (status, artifact, message) no longer ring your bell, and a peer's repeated `working` with no text no longer reaches your bell or `poll`. `agent-gossip bell-check` exits 3 when it blocks. After you upgrade, the hook refuses the first stop of a live session once, because the bell armed by the old binary holds no lock; re-arm it and continue.
