---
type: scenario-runbook
title: Cross-harness
description: A pi peer and a Claude Code peer collaborate in one mesh; the experience should match.
tags: [cross-harness, pi, claude-code, parity, interop]
timestamp: 2026-06-28T00:00:00Z
roles: [pi-peer, cc-peer]
coordinator: dedicated
harness: cross
prereqs: [agent-gossip]
network: private
---

# Cross-harness

## Scenario

Two peers on different front-ends — one pi, one Claude Code — share a single
mesh and collaborate across messaging, a task, and a couple of shared-state
moves. The point is that they interoperate and that the **experience looks the
same on both front-ends**. Set up per the [coordinator protocol](/coordinator.md).

## Roles & goals

- **pi-peer** (runs the pi extension) — collaborate with the other peer: send a
  message, delegate a small task to it, and make a move in the shared document.
- **cc-peer** (runs the Claude Code plugin) — collaborate back: reply, run the
  task and return its result, and make the counter-move.

Swap the harnesses on a second run to check the reverse direction.

## Briefing

- mesh: `scenario-cross`
- a tiny shared document for the state step: `{ "turn": "a", "n": 0 }`
- harness assignment: `pi-peer` on pi, `cc-peer` on Claude Code

## Expected behavior & UX

- [ ] messaging, the task, and the shared-state moves all complete across the
      two harnesses
- [ ] the shared document converges identically on both sides
- [ ] the rendered lines match across front-ends — presence, a message, a
      directed reply, and a shared-state change read the same on pi and Claude
      Code (same wording, same `💬️` glyph; a mesh id is `💬://<base58>`)
- [ ] no harness-specific desync: nothing shown on one peer is missing on the
      other
