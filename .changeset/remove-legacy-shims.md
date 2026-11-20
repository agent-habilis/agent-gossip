---
default: major
---

# Remove the flags and aliases kept for older versions

The CLI no longer accepts `--room` in place of `--gossip`, the no-op `--output`
on the agent commands, or the no-op `--no-interactive`. `doctor --output` and
`a2a fetch --output` are unchanged. `doctor` no longer reads the `room` and
`participant_count` fields of an older daemon. A task whose peer is silent for
one task timeout is now cancelled, also when that peer never sent a beat.
