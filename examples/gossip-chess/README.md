# gossip-chess

A **custom skill** — one nobody ships, written against the same substrate the
built-in `gossip-*` skills use. Two agents in a gossip play a game of chess
against each other: the board lives in the gossip's shared state, the game is
one A2A task, and each player prints the position in its own chat every turn.

Where [`a2a-interop`](../a2a-interop) proves the A2A surface is
implementation-generic, this example makes a smaller point about the layer
above it: **the binary has no idea what a skill is.** A skill is a markdown
procedure that drives the CLI, so anything you can express as `agent-gossip`
calls plus a receive loop is one — including a two-player game the binary has
never heard of.

```
challenger                     shared state (/chess)                    opponent
    │                                   │                                    │
    ├─ SendMessage (no --task-id) ──────┼──────────────────────────────────▶ │  task opens
    │                                   │                              ◀──── ├─ status working  (accept · wakes)
    ├─ state merge {fen,prev,turn:"b"} ▶│                                    │  the record — wakes no one
    ├─ SendMessage --task-id "e4" ──────┼──────────────────────────────────▶ │  the signal — wakes
    │                                   │◀─ state merge {fen,prev,turn:"w"} ─┤  the record — wakes no one
    │                                   │                              ◀──── ├─ status working "e5"  (wakes)
    │                            … game …                                    │
    │                                   │                              ◀──── ├─ artifact (PGN)
    ├─ SendMessage --task-id (approve) ─┼──────────────────────────────────▶ │
    │                                   │                              ◀──── ├─ status completed
```

The two kinds of arrow are the whole design. The **merge** is the record — what
the position *is*. The **task leg** is the signal — what wakes the other player.
A state merge reaches the peer and is readable in its next batch, but it never
rings the bell, so a game built on merges alone deadlocks on move one.

## The shared state is the game

Everything about the game — position, turn, colors, move history, result — is
one key in the gossip's shared state document, `/chess`. Nothing about it lives
in either agent's context, in chat, or on the task. The task carries the turn
signal, liveness, and ceremony; chat carries what the humans read; both are
*derived from* `/chess`, never the reverse.

That constraint is the point of the example, and it is what a single
`agent-gossip state get` buys:

- a player whose context was cleared mid-game resumes from one read — task id,
  its own color, the position, the history, whose move it is
- a peer joining halfway sees the whole game with no replay
- a third member spectates without being told anything
- the two players never disagree about the position, because there is only one

The move history is an object keyed by ply, not an array, so a merge adds one
half-move and leaves the rest alone — RFC 7386 has no array append, and an
array would be overwritten on every move.

## What it demonstrates

| piece | substrate it uses |
|---|---|
| the board | the **shared state** document — a CRDT every member folds identically, under one namespaced key, `/chess`, and the game's only record |
| whose turn it is | a **turn marker** in that document — the mutual exclusion between two writers touching the same keys. Not the wake signal: a document echo never rings the bell |
| the move history | an **object keyed by ply**, because RFC 7386 has no array append — a merge naming an array key replaces the whole array |
| the game session | one **A2A task**, opened by the challenge and closed by the result. Accept/decline is the task's `working` / `failed`; the PGN is its artifact |
| the wake, and liveness | a **task leg** with every move, carrying the SAN. It is what rings the peer's bell, and it resets the task's ~2-minute idle eviction — a state merge does neither |
| the ceremony | the leg's form follows the **A2A role, not the color**: only a task's server may author a status, so the opponent moves with `a2a status` and the challenger with `a2a call --task-id`. Same rule makes the opponent post the artifact and author `completed` even when it loses |

## What it does *not* carry

The skill is around three hundred lines, where a built-in runs to six or seven
hundred, because it inherits everything about being in a gossip from the session that already
ran `/gossip-join`: the daemon, the bell and receive-loop contract, event
handling, task tracking in the todo widget, the question widget, `$GOSSIP` /
`$NICKNAME` / `$SKILL_PREFIX`. It declares that as a prerequisite instead of
restating it.

The built-ins get their copy of that contract inlined at build time — the
`<!-- include path="../shared/…" -->` directives in `skills/*/SKILL.md` are
expanded by `slot-template` in
[`crates/agent-gossip/build.rs`](../../crates/agent-gossip/build.rs). That
machinery is not available to a hand-written skill, and a skill meant to run
outside a live gossip session would have to carry the relevant sections itself.

It also costs the example a piece of plumbing worth knowing about. A built-in's
worker flow is inlined into *every* skill, so a peer's task brief always lands
somewhere that knows what to do with it. A hand-written skill only activates on
what its own `when_to_use` matches — so this one names the inbound challenge
there explicitly, and its brief names the skill back. Only the challenger types
a command; the opponent is activated by the brief.

## Install it

Copy it into any harness's skill root — the same roots `agent-gossip plug`
writes to, which `agent-gossip doctor` prints:

```sh
# Claude Code
cp -r examples/gossip-chess ~/.claude/skills/gossip-chess

# Codex ~/.codex/skills · pi ~/.pi/agent/skills · Cursor ~/.cursor/skills
# opencode ~/.config/opencode/skills
```

`unplug` removes only the `gossip-*` directories it installed itself, so a
custom skill sitting beside them survives `plug` and `unplug` alike. Pick a
name a future release is unlikely to ship, though — a built-in would shadow it.

## Play it

Two agent sessions, in any harnesses, on any machines:

```text
session A: /gossip-create
session B: /gossip-join <hash>
session A: /gossip-chess <peer>
```

Session B is asked to accept. From there the game runs on its own, a board
printing in both chats every turn:

```text
   a b c d e f g h
 8 ♜ ♞ ♝ ♛ ♚ ♝ ♞ ♜ 8
 7 ♟ ♟ ♟ ♟ ♟ ♟ ♟ ♟ 7
 6 · · · · · · · · 6
 5 · · · · · · · · 5
 4 · · · · ♙ · · · 4
 3 · · · · · · · · 3
 2 ♙ ♙ ♙ ♙ · ♙ ♙ ♙ 2
 1 ♖ ♘ ♗ ♕ ♔ ♗ ♘ ♖ 1
   a b c d e f g h
```

Any third member can spectate with `agent-gossip state get` — `/chess` holds
the whole game.

## The honest limitation

There is no chess engine here. Both players are language models reading a FEN,
and a model will eventually claim a move that is not legal in the position. The
receiving side checks each move against the previous FEN and, on a mismatch,
merges a dispute and hands the turn back rather than answering it. That is a
guardrail, not a referee — the skill is a demonstration of the substrate, not a
strong chess program.
