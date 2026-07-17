---
type: scenario-runbook
title: Shared state — Connect Four
description: Two players play Connect Four over the shared-state document.
tags: [state, shared-state, game, connect-four, turn-marker]
timestamp: 2026-06-28T00:00:00Z
roles: [player-a, player-b]
coordinator: dedicated
harness: any
prereqs: [agent-gossip]
network: private
---

# Shared state — Connect Four

## Scenario

Two players play a full game of Connect Four over the mesh's shared-state
document — longer than tic-tac-toe, shorter than checkers. Set up per the
[coordinator protocol](/coordinator.md): the coordinator broadcasts the briefing
below by message; the live game data lives in the mesh's shared-state document,
which the players create and mutate.

## Roles & goals

- **player-a** — play Connect Four as player `a`. You move first: get the game
  started, then play it to a finish.
- **player-b** — play Connect Four as player `b`.

## Briefing

- mesh: `scenario-connect-four`
- **Document model** the players share:
  ```json
  {
    "game": "connect-four",
    "players": { "a": "<player-a>", "b": "<player-b>" },
    "board": { "c0": [], "c1": [], "c2": [], "c3": [], "c4": [], "c5": [], "c6": [] },
    "turn": "a",
    "status": "playing"
  }
  ```
- **Rules:** a disc is `"a"` or `"b"`; a move drops your disc onto a column
  (`c0`–`c6`), stacking bottom-to-top; a column holds at most 6. Players
  alternate, tracked by `turn`. `status` is `playing | a-wins | b-wins | draw`.
  A win is four of your discs in a row — horizontal, vertical, or diagonal.

## Expected behavior & UX

- [ ] the game starts and both players converge on the same starting board
- [ ] players alternate turns; each move surfaces to the other player as it
      happens, with the updated board
- [ ] the board converges identically on both sides after every move
- [ ] the game ends with the correct `status` (a win or a draw), and both sides
      agree on the outcome
- [ ] the experience is turn-by-turn and legible in each player's UI
