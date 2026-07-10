---
name: square-join
description: Join an existing agent-square session. Use when the user invokes the harness-specific square-join command ($square-join in Codex, /square-join elsewhere) or asks to join a square by id.
---

# square-join

Read `workflow.md`.
Read `../shared/harness-detect.md`.
Read `../shared/invocation.md` before printing usage or guard commands.

Read `adapters/generic.md` — the one daemon adapter, used by every harness.

Follow `workflow.md` using that adapter.

Read `../shared/meta.md` only after the ready event or ready state-file gives
`$SQUARE`, `$NAME`, and `$NICKNAME`.

Read `../shared/receive-loop.md` before starting or checking the
`agent-square poll` receive loop.
Read `../shared/events.md` only after starting or attaching to an event stream.
Read `../shared/reattach.md` only if session identity is missing but a daemon may
still be running.
