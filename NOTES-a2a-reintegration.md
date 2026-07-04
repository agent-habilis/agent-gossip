# Native A2A reintegrated onto origin/main — DONE (green)

Your native-A2A + v1.0 work, rebased onto the current `origin/main` and
reconciled so the whole codebase speaks the native A2A model ("move all to
a2a"). `cargo task ci` is green: fmt clean, clippy `--all-targets --features
adversarial` 0 warnings, 492 lib tests, adversarial suite, and the full
subprocess/reliability suite all pass.

## What the reconciliation did
- **Rebase**: squashed native-A2A + v1.0 onto `origin/main`, resolved all 47
  conflicts to the A2A files, then migrated origin's non-conflicted files
  forward off the old model.
- **Dropped the old chat model**: removed `Msg`/`Notice`/`Task` commands and
  their handlers; the messaging surface is `a2a call` / `status` / `artifact`.
- **Merged the two `a2a` subcommand trees**: origin's HTTP tunnel
  (`expose`/`connect`/`discover`) and native messaging (`call`/`status`/
  `artifact`) under one `A2aAction`.
- **Kept origin's features**: the A2A HTTP tunnel, the `💬://` swarm-id URI
  form, the swarm-relay rung, and the pipe/file/port/sh/mount removals.
- **Fixed the format-drift fallout**: swarm-id literals and the state-file path
  helper now use the `💬://` form + `swarm_prefix` scheme-strip; the
  adversarial kind-tamper attack retargeted to `A2aMsg`↔`Presence`.

## Deferred (one clean follow-up)
- **Unicast transport (origin's point-to-point optimization) is removed, not
  wired.** Directed A2A frames (`A2aStatus`/`A2aArtifact`/`A2aReq`/`A2aResp`/
  `Pong`) currently flood + heal over gossip — correct, just not the 1:1
  optimization. Re-adding it means wiring origin's `src/unicast/` acceptor +
  `unicast_rx` drain into *this* event loop and routing directed frames by
  `sole_addressee` in the outbound path. The two hooks it needs
  (`recv::ingest`, `protocol::message::sole_addressee`) were added then removed
  with the module; re-add both when wiring it. Orthogonal to the A2A model.

## Finalizing
This work is on the working tree of `wip/a2a-on-origin`. To land it:
`git commit` it, then fast-forward `main` (or open a PR). `main` still holds
your original two commits untouched.
