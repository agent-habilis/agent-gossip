## 0.10.0 (2026-09-24)

### Breaking Changes

- 

### Fixes

#### Refuse a turn that leaves the gossip bell unarmed

On Claude Code, the gossip skills now register a Stop hook. The hook refuses to end a turn while this agent's session has no bell armed. `agent-gossip session` reports `"bell": true/false`. `poll --long` takes a new `--settle-secs` flag. Your own `working` status updates no longer ring your bell.
