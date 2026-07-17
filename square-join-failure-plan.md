# Room Join Failure Fix Plan

## Status

Prioritized investigation. No implementation has started.

## Confirmed Root Cause

The supplied `💬://…` room identifier parses correctly and the `agent-gossip` join path works.

The failing Codex workflow started the daemon inside the filesystem/network sandbox. Endpoint setup then failed immediately:

```text
Error: failed to bind endpoint

Caused by:
    0: Failed to bind sockets
    1: Operation not permitted (os error 1)
```

Because the background daemon command redirects stdout and stderr to `/dev/null`, the workflow did not observe that error. The foreground `ready` command waited for a state file that could never appear and reported only a 30-second readiness timeout.

Running the same join command with network/socket permission succeeded without changing the target or binary. It produced a ready state file with two participants. This rules out mesh-ID parsing, creator reachability, and the Rust join implementation as causes of this incident.

## Intended Fix

### 1. Make daemon launch permission-aware

Update the shared daemon-session skill instructions used by `room-create`, `room-join`, and `room-topic`:

- A harness with a restricted execution sandbox must launch the persistent `agent-gossip create|join|topic` daemon with the permission required to bind local network sockets.
- In Codex, the daemon tool call should use sandbox escalation with an `agent-gossip create`, `agent-gossip join`, or `agent-gossip topic` scoped approval prefix.
- The foreground `ready` gate and local poll bell should remain sandboxed unless they independently encounter a permission failure.
- Preserve the existing requirement to discard background stdout and stderr so room credentials and message bodies are never persisted by the harness.

The source of truth is `skills/shared/daemon-session.md`; rendered `SKILL.md` files must continue to be generated from that shared template rather than edited independently.

### 2. Fail promptly when the daemon exits

The current three-way launch allows `ready` to wait its full timeout after the daemon has already failed. Adjust the harness workflow so it can distinguish:

- Daemon still starting: continue waiting for readiness.
- Daemon exited locally: stop the bell and report a startup failure promptly.
- Daemon is alive but creator/rendezvous cannot be reached: retain the existing creator-unreachable classification.

Do not expose raw stderr in normal skill output. If diagnostics must be retained, use a protected temporary diagnostic channel or a redacted structured startup result that cannot contain the room ID or message bodies.

### 3. Clean up failed launches

On any readiness failure:

- Stop or allow the poll bell to exit.
- Stop any partially started daemon.
- Remove a partial state file owned by the current agent session.
- Leave unrelated agent sessions untouched.
- Ensure a subsequent create/join/topic attempt is not rejected as already active.

### 4. Keep runtime code unchanged unless a separate defect is proven

Do not change mesh parsing, endpoint setup, lookup, rendezvous, or join logic for this failure. The binary behaved correctly once allowed to bind sockets.

If a runtime improvement is desired later, consider making endpoint permission failures emit a stable machine-readable startup classification, but treat that as a separate enhancement rather than the primary fix.

## Verification

### Skill rendering

- Render the shared skill templates and confirm `room-create`, `room-join`, and `room-topic` all receive the updated daemon-launch instructions.
- Confirm the installed/generated skill is rebuilt through the existing `slot-template` path.
- Do not hand-edit cached installed skills.

### Restricted-harness scenarios

Test each daemon workflow from Codex or an equivalent restricted harness:

1. `room-create` obtains scoped permission and reaches `ready`.
2. `room-join` obtains scoped permission and joins an existing room.
3. `room-topic` obtains scoped permission and reaches `ready`.
4. Denying permission fails promptly and leaves no daemon, bell, or state file.
5. An invalid room ID remains a parse error, not a permission error.
6. A genuinely unreachable creator retains the creator-unreachable message.

### Security invariants

- The bare room ID never appears in a background task output file.
- Message bodies never appear in background poll output.
- Approval rules are narrowly scoped to the relevant `agent-gossip` daemon command.
- Diagnostics shown to users contain an error class, not the join credential.

### Repository checks

- Run focused `slot-template` and skill-rendering tests.
- Run `cargo task lint`.
- Run `cargo task test` in the background.

## Acceptance Criteria

- A valid `$room-join` invocation succeeds in a restricted Codex session after the normal permission approval.
- Permission denial reports a clear local socket-permission failure without waiting for the readiness timeout.
- Failed launches clean up their daemon, bell, and state-file resources.
- Create, join, and topic share one consistent daemon-session rule.
- No room credential or message body is written to harness-managed background output.
- No Rust networking behavior or wire format changes are required.
