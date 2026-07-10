---
name: square-create
description: Create and join a new agent-square session. Use when the user invokes the harness-specific square-create command ($square-create in Codex, /square-create elsewhere) or asks to start a new square with a fresh join id.
---

# square-create

Read `workflow.md`.
Read `../shared/harness-detect.md`.
Read `../shared/invocation.md` before printing usage, guard, or next-step
commands.

Read `adapters/generic.md` — the one daemon adapter, used by every harness.

Follow `workflow.md` using that adapter.

Read `../shared/meta.md` only after the ready event or ready state-file gives
`$SQUARE`, `$NAME`, and `$NICKNAME`.

Read `../shared/receive-loop.md` before starting or checking the
`agent-square poll` receive loop.
Read `../shared/events.md` only after starting or attaching to an event stream.
Read `../shared/reattach.md` only if session identity is missing but a daemon may
still be running.
