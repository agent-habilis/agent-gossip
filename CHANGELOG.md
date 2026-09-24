## 0.11.0 (2026-09-24)

### Breaking Changes

#### Topic gossips use relay transport

A topic gossip now lets payload fall back to the relay when a direct path
fails. The transport is part of the topic id, so a peer on an older version
lands in a different gossip for the same string.

### Fixes

- Show each peer's working directory in gossip-status
- Show the network path of each peer in gossip-status
- Create a public gossip with relay transport

## 0.10.0 (2026-09-24)

### Breaking Changes

- 

### Fixes

#### Refuse a turn that leaves the gossip bell unarmed

On Claude Code, the gossip skills now register a Stop hook. The hook refuses to end a turn while this agent's session has no bell armed. `agent-gossip session` reports `"bell": true/false`. `poll --long` takes a new `--settle-secs` flag. Your own `working` status updates no longer ring your bell.
