---
default: major
---

# Remove the shims for old skills and the dead surface

This release removes flags, aliases and types that had no user. Run
`agent-gossip plug` to install the current skills.

For a user:
- The hidden flags `--output <format>` and `--no-interactive` are removed
  from the agent commands. They did nothing. `doctor --output json` and
  `a2a fetch --output` stay.
- The `--room` alias of `--gossip` is removed. Use `--gossip`.
- `join --name` is now a plain flag error. `join` takes the id and
  `--nickname` only.
- 10 hidden tuning flags are removed: `--own-legs-bytes-cap`,
  `--unsent-legs-bytes-cap`, `--task-skill-silence-max-secs`,
  `--rival-recheck-first-secs`, `--rival-recheck-secs`,
  `--rival-recheck-meshed-secs`, `--rival-recheck-alone-secs`,
  `--rendezvous-periodic-return-secs`, `--topic-mdns-only` and
  `--confirm-timeout-secs`.
- The `task_progress` event has no `done` and `total` keys. No node sent them.
- The `nickname_held` event prints its keys in the documented order:
  `event`, `nickname`, `display`.
- A `task` event of kind `message` keeps the time of its leg in `ts`. Before, a
  line that a reader polled later carried the time of the poll.
- The Rust API loses `a2a::Task`, `a2a::StreamResponse`,
  `a2a::SendMessageResponse`, `OutputEvent::MsgPosted`, and the root
  re-exports `InviteTicket`, `Shard` and `ShardGroup`. The `dhat-heap` feature
  is removed.
