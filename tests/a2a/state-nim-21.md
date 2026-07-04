---
type: a2a-runbook
title: Shared state — Nim "21"
description: Two players play the subtraction game "21" over the shared-state document.
tags: [state, shared-state, game, nim, turn-marker]
timestamp: 2026-06-28T00:00:00Z
roles: [player-a, player-b]
coordinator: dedicated
harness: any
prereqs: [agent-gossip]
network: private
---

# Shared state — Nim "21"

## Scenario

Two players play the classic subtraction game "21" over the swarm's shared-state
document — a fast game that ends in ~7–15 moves, shorter than Connect Four.
Set up per the [coordinator protocol](/coordinator.md): the coordinator
broadcasts the briefing below by message; the live game data lives in the
swarm's shared-state document, which the players create and mutate.

## Roles & goals

- **player-a** — play "21" as player `a`. You move first: get the game started,
  then play it to a finish.
- **player-b** — play "21" as player `b`.

## Briefing

- swarm: `a2a-nim-21`
- **Document model** the players share:
  ```json
  {
    "game": "nim-21",
    "players": { "a": "<player-a>", "b": "<player-b>" },
    "pile": 21,
    "turn": "a",
    "status": "playing"
  }
  ```
- **Rules:** the pile starts at 21. On your turn you take 1, 2, or 3 from the
  pile (you may not take more than remains). Players alternate, tracked by
  `turn`. The player forced to take the **last** counter — the move that brings
  `pile` to 0 — **loses**; the other player wins. `status` is
  `playing | a-wins | b-wins`.

## Expected behavior & UX

- [ ] the game starts and both players converge on the same starting pile (21)
- [ ] players alternate turns; each move surfaces to the other player as it
      happens, with the updated pile and whose turn is next
- [ ] the pile converges identically on both sides after every move, never going
      below 0 and never skipping a turn
- [ ] the game ends with the correct `status` (the player who took the last
      counter loses), and both sides agree on the outcome
- [ ] the experience is turn-by-turn and legible in each player's UI
