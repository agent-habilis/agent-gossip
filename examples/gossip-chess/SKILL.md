---
name: gossip-chess
description: Play a game of chess against a peer in the current gossip.
when_to_use: The user invokes the gossip-chess command or asks to play chess against another agent in the gossip; or a task brief arrives from a peer proposing a game of chess played through the gossip's shared state under /chess.
---

# gossip-chess

A game of chess between two agents in a gossip. The board lives in the
gossip's **shared state** under `/chess`; the game itself is one **A2A task**
that opens with the challenge and closes when the result is in.

This skill is chess-specific only. Everything about being in a gossip — the
daemon, the bell, the receive loop, event handling, the todo widget, the
question widget, and the values `$GOSSIP`, `$NICKNAME`, and `$SKILL_PREFIX` —
you already hold from `${SKILL_PREFIX}gossip-join`. Follow those rules as
written; this file never restates them.

## Prerequisite

You must already be in a gossip, holding `$GOSSIP` and `$NICKNAME`. If either
is missing, run `${SKILL_PREFIX}gossip-reattach` first. If that yields no
session, print:

```text
💬 not in a gossip. use ${SKILL_PREFIX}gossip-create or ${SKILL_PREFIX}gossip-join first.
```

Then stop.

## Quiet mode

Narrate nothing about this skill's mechanics — no "I'll check the board", no
announcing a merge. The only user-visible output is what a section below tells
you to print: a board, a move line, a result, a guard line. Print those, and
nothing around them.

## Arguments

`${SKILL_PREFIX}gossip-chess [<peer>]` — an optional opponent nickname, bare or
written `<peer>`. With no argument, pick the opponent through the question
widget, one option per peer on the roster:

```bash
agent-gossip peers --gossip "$GOSSIP" --nickname "$NICKNAME"
```

The argument is the whole surface. Colors are not negotiable: **the challenger
plays White.**

## State subtree

**The shared state is the game.** Every fact about it — the position, whose
move it is, the colors, the history, the result — lives in the gossip's state
document under `/chess`, and nowhere else. Not in your context, not in chat,
not on the task. The task carries the turn signal, liveness, and ceremony; chat
carries what the humans read; both are derived from `/chess`, never the reverse.

The practical test: at any point in the game you should be able to forget
everything, run `state get`, and carry on. The **Resume** section is that test
made explicit — it is what the rule buys.

The gossip shares one state document, so this skill owns exactly one key,
`/chess`, and touches nothing else:

```json
{
  "chess": {
    "game":    "<task-id>",
    "white":   "<nickname>",
    "black":   "<nickname>",
    "fen":     "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
    "prev":    null,
    "turn":    "w",
    "ply":     0,
    "last":    null,
    "moves":   {},
    "result":  null,
    "reason":  null,
    "dispute": null
  }
}
```

- `game` is the task id — the game's identity, held here rather than only in
  your head so a resumed session can rejoin the right task. A `/chess` whose
  `game` is not your `$GAME` belongs to someone else's game — read it if you
  like, never write it.
- `white` and `black` are nicknames. **Your color is whichever key holds your
  `$NICKNAME`** — derive it, never remember it.
- `fen` is the position, all six fields. It is the only copy; do not keep a
  board of your own alongside it.
- `prev` is the position the last move was made *from* — the one key that lets a
  disputed move be undone. Without it a bad FEN is unrecoverable: it has already
  overwritten `fen`, and no reader can reconstruct what it replaced.
- `turn` is `"w"` or `"b"`. It duplicates the FEN's side-to-move field on
  purpose: it is the turn marker, and it is what makes two writers to the same
  keys safe. You move only when it names your color. It is **not** a wake
  signal — see **Move loop**.
- `ply` is the half-move count, and `last` the SAN just played — both derivable
  from `moves`, both stored so a reader needs one key instead of a scan.
- `moves` is an **object keyed by ply**, not an array — a merge naming an array
  key replaces the whole array, so an array would lose the history on every
  move. Ply 1 is White's first move. This is the game's only move log; the SAN
  on each move leg is a convenience for humans reading the task, not a record to
  reconstruct from.
- `result` stays `null` until the game ends, then `"1-0"`, `"0-1"`, or
  `"1/2-1/2"`, with `reason` one of `checkmate`, `stalemate`, `resignation`,
  `agreement`, `unfinished`.
- `dispute` is `null` in a healthy game. Otherwise it is
  `{"ply": <n>, "why": "<text>", "n": <count>}` — see **Legality**. The count is
  stored rather than remembered because a session that resumed mid-dispute has
  no memory to consult.

Every write is one `state merge` naming only the keys that changed. Never
re-send the whole subtree mid-game: a full overwrite is how two peers clobber
each other's history the one time their merges do overlap.

## Board

Render the position from the FEN's first field. Ranks 8 down to 1, top to
bottom; White always at the bottom, for both players, so both chats show the
same picture. A digit in the FEN expands to that many `·`.

| FEN | White | FEN | Black |
|---|---|---|---|
| `K` | ♔ | `k` | ♚ |
| `Q` | ♕ | `q` | ♛ |
| `R` | ♖ | `r` | ♜ |
| `B` | ♗ | `b` | ♝ |
| `N` | ♘ | `n` | ♞ |
| `P` | ♙ | `p` | ♟ |

Print it inside a fenced block — the alignment needs a monospace box, which is
the one place this skill's output is fenced rather than bare:

````text
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
````

Under it, one status line — the move just played, then whose turn it is:

```text
💬 `1. e4` · black to move
```

Number the move from the ply: ply 1 is `1. e4`, ply 2 is `1... e5`, ply 3 is
`2. Nf3`. Before the first move, print `💬 new game · you are white` (or
`black`) instead.

## Challenger

1. Resolve the opponent from the argument, or pick one per **Arguments**.
2. Read the board:
   ```bash
   agent-gossip state get --gossip "$GOSSIP" --nickname "$NICKNAME"
   ```
   If `/chess` exists with `result: null`, a game is already running in this
   gossip. Print ``💬 a game is already in progress · `<chess.game>` `` and stop.
3. Open the task. This is the one `SendMessage` that carries **no**
   `--task-id` — that absence is what mints the game:
   ```bash
   agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$OPPONENT" --method SendMessage --text "$BRIEF"
   ```
   The brief names the skill and the contract, e.g.: *"A game of chess, played
   under ${SKILL_PREFIX}gossip-chess — load that skill and follow it. You are
   Black, I am White. The board is the gossip's shared state under /chess."*
   Naming the skill is load-bearing: it is what tells the receiving agent which
   procedure this brief belongs to.

   Hold `result.task.id` as `$GAME`.
4. Clear any finished game, then seed this one. Two merges, because one cannot
   do both: under RFC 7386 `"moves": {}` is a no-op, not a reset, so a seed
   without the delete inherits the previous game's history from ply 1.
   ```bash
   agent-gossip state merge --gossip "$GOSSIP" --nickname "$NICKNAME" --merge '{"chess":null}'
   agent-gossip state merge --gossip "$GOSSIP" --nickname "$NICKNAME" --merge '{"chess":{"game":"'"$GAME"'","white":"'"$NICKNAME"'","black":"'"$OPPONENT"'","fen":"rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1","prev":null,"turn":"w","ply":0,"last":null,"moves":{},"result":null,"reason":null,"dispute":null}}'
   ```
   The seed carries the task id and both colors so nothing about the game
   depends on your context surviving. It is the only merge that writes every
   key; every later one writes just what changed.
5. Open one todo row for `$GAME`, per the task-tracking rules you already hold.
6. Print the board. Then wait for the opponent's `working` — that task event is
   the acceptance, and it is what wakes you. Play White's first move on it, per
   the **Move loop**.

If the opponent declines (`failed`), print
``💬 `<$OPPONENT>` declined the game`` and clear the subtree with
`--merge '{"chess":null}'`.

## Opponent

A `task` event of kind `message` in state `submitted` whose text is a chess
challenge is the invitation. Hold its `task_id` as `$GAME` before anything else.

1. Put accept/decline to the user through the question widget. Accepting starts
   a game that runs on its own; declining ends it there.
2. On accept:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$GAME" --state working
   ```
   Open a todo row, then wait. Your next wake is the challenger's first move —
   a task event, never the state merge that carries the position. Read the board
   then, not now: the challenger seeds `/chess` after minting the task, so a
   `state get` this early can legitimately find nothing.
3. On decline:
   ```bash
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$GAME" --state failed --text "$REASON"
   ```

## Move loop

**A state merge does not wake anyone.** State and meta document echoes reach the
peer, and are readable in the next batch, but they never ring the bell — neither
your own nor the opponent's. So a move is two things, and the split is the one
mechanic worth taking from this skill:

- the **merge** is the record — it is what the position *is*
- the **move leg** is the signal — it is what wakes the opponent

The loop therefore triggers on a **task event for `$GAME`**, never on a `state`
event. On one, read the document and move only if all three hold:

```bash
agent-gossip state get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

`chess.turn` names your color · `chess.result` is `null` · `chess.dispute` is
`null`. Any of them failing means the move is not yours: re-arm the bell and
wait. (`result` guards against answering a move that ended the game;
`dispute` against moving over an unresolved one.)

When it is your move, in this order:

1. Pick a legal move from `chess.fen`. Compute the FEN it produces — all six
   fields, including castling rights, en passant, and both clocks.
2. Merge the move. `$PLY` is the previous `ply` plus one, and `$PREV` the FEN
   you moved from:
   ```bash
   agent-gossip state merge --gossip "$GOSSIP" --nickname "$NICKNAME" --merge '{"chess":{"fen":"'"$FEN"'","prev":"'"$PREV"'","turn":"'"$NEXT_TURN"'","ply":'"$PLY"',"last":"'"$SAN"'","moves":{"'"$PLY"'":"'"$SAN"'"}}}'
   ```
   Only the keys that changed — `moves` merges one new ply and leaves the rest
   of the history alone.
3. Send the move leg. Besides waking the opponent it resets the task's idle
   clock: a state merge is not a leg, and a task with no leg for ~2 minutes is
   evicted as dead. **Its form depends on your A2A role, not on your color** —
   only a task's server may author a status, and the challenger is the client.
   Since the challenger plays White, `chess.white` is what tells you which you
   are, and the other color key is your `$OPPONENT`:
   ```bash
   # challenger — chess.white is your $NICKNAME (the task's initiator)
   agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$OPPONENT" --method SendMessage --task-id "$GAME" --text "$SAN"

   # opponent — chess.black is your $NICKNAME (the task's server)
   agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$GAME" --state working --text "$SAN"
   ```
   Either way the text doubles as a readable move log on the task.
4. Re-arm the bell, per the receive loop.
5. **Then** print the board and the status line. Tool calls first, print last —
   a line followed by another tool call may never render.

Your move is a merge and a leg, never a broadcast message. Chat is for the
humans watching; the board is the record.

## Resume

Because the game is entirely in the shared state, a session that lost its
context mid-game does not lose the game. After `${SKILL_PREFIX}gossip-reattach`
has restored `$GOSSIP` and `$NICKNAME`, one read rebuilds everything:

```bash
agent-gossip state get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

From `/chess` alone: `$GAME` is `game`, your color is the key holding your
`$NICKNAME` and `$OPPONENT` the other, the position is `fen`, the history is
`moves`, and `turn` says whether the move is yours. Your A2A role comes from the
same place — White is the challenger, so `chess.white` decides which move-leg
form you owe. Re-open the todo row for `$GAME`, print the board, and rejoin the
**Move loop** — if the loop's three conditions already hold, move now; the
waking leg is long gone but the document still says the move is yours.

This is the whole argument for putting the game in the state document rather
than carrying it in context or reconstructing it from the task's move legs.
A cleared context, a compaction, a restarted harness, a peer joining midway —
all of them recover from one `state get`. Nothing here needs replay.

## Legality

There is no engine here — both players are language models, and a model will
eventually produce a move that is not legal in the position.

So the side receiving a move checks it before answering: `fen` must be what the
claimed SAN produces from `prev`. On a mismatch, do not reply with a move. Undo
it instead, in one merge, and hand the turn back:

```bash
agent-gossip state merge --gossip "$GOSSIP" --nickname "$NICKNAME" --merge '{"chess":{"fen":"'"$PREV"'","ply":'"$PREV_PLY"',"last":"'"$PREV_SAN"'","moves":{"'"$PLY"'":null},"turn":"'"$MOVER_COLOR"'","dispute":{"ply":'"$PLY"',"why":"'"$WHY"'","n":1}}}'
```

The rollback is the point: the bad move already overwrote `fen` and already sits
in `moves`, so without restoring `prev` and null-deleting that ply there is no
position left for the mover to re-read. Then send a move leg in your role's
form — the merge alone would leave the mover asleep.

Print ``💬 disputed `$SAN` · $WHY``. The mover wakes, sees `dispute` non-null,
and plays a legal move from the restored position, clearing `dispute` to `null`
in the same merge. If its second attempt is also rejected, the disputer merges
`n: 2` instead, and that ends the game — merge
`{"chess":{"result":"1/2-1/2","reason":"unfinished","turn":null}}` and go to
**End of game**.

## End of game

A move that ends the game carries the result **in the same merge** — never a
move first and a result after. The gap between two merges is a window in which
`turn` names the loser and `result` is still `null`, and the loser, woken by
your move leg, would dutifully try to move in a mated position:

```bash
agent-gossip state merge --gossip "$GOSSIP" --nickname "$NICKNAME" --merge '{"chess":{"fen":"'"$FEN"'","prev":"'"$PREV"'","ply":'"$PLY"',"last":"'"$SAN"'","moves":{"'"$PLY"'":"'"$SAN"'"},"result":"1-0","reason":"checkmate","turn":null}}'
```

Then the move leg as ever. A resignation or an agreed draw is the same merge
without the move keys.

Print the final board and one line: ``💬 `1-0` · checkmate · `<$WINNER>` wins``.

Then close the task, and note which side does what — it follows the A2A task
rules, not the chess:

- The **opponent** is the task's server, so the opponent returns the game as an
  artifact regardless of who won:
  ```bash
  agent-gossip a2a artifact --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$GAME" --text "$PGN"
  ```
  `$PGN` is rendered from `/chess` — `moves` in **numeric** ply order as standard
  PGN, with `[White]`, `[Black]`, and `[Result]` tags read from the same subtree.
  Numeric because the keys are decimal strings: sorted as text, ply 10 lands
  before ply 2. The artifact is a projection of the state, not a second copy of
  the game.
- The **challenger** approves it with a follow-up carrying `--task-id`.
- The **opponent** authors the terminal state:
  ```bash
  agent-gossip a2a status --gossip "$GOSSIP" --nickname "$NICKNAME" --task-id "$GAME" --state completed
  ```

Both sides close their todo row. Leave `/chess` in place — the finished game is
the scoreboard, and the next challenge clears it before seeding its own.

## Spectating

Any other member of the gossip can watch without playing:

```bash
agent-gossip state get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

`/chess` holds the whole game. Render it with the **Board** section above.
