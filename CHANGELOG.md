## 0.11.1 (2026-09-25)

### Features

#### Keep a task alive while both daemons run

Both daemons now beat every live task from the offer on, whoever holds the
ball. A party cancels a task only after 2 minutes with no leg or beat from the
other daemon. A pending accept question, a slow review, or a long build no
longer cancels the task at about 4 minutes, and skills no longer re-emit
`working` to keep a task alive. A task that neither agent touches for 24 hours
stops being beaten, and the other daemon cancels it about 2 minutes later, so
a task forgotten after a `/clear` still ends. A peer
on an older version is not cancelled for silence until it has sent a beat.

### Fixes

#### Wait for the daemon's slower shutdown in leave

The engine now takes up to about 6 s to close its rendezvous links on shutdown, up from about 3 s. `agent-gossip leave` waited only 5 s by default for the daemon to exit, so on a slow relay it could report a failed leave, or let a relaunch race a daemon that was still serving. It now waits the engine's leave budget plus 2 s (8 s). A leave can therefore take a few seconds longer.

#### Never surface a replayed chat message

Anti-entropy could serve an old `msg` or broadcast again after the engine forgot its id, up to hours later, as a new visible line. The daemon now remembers the chat it surfaced, keyed like the engine's own dedup (author key and message id), and keeps a repeat off the surface. A message from another author that reuses the id still surfaces. A daemon restart forgets what it surfaced, so one more replay can show after a restart.

#### Never surface a replayed task leg

Anti-entropy could serve an old task leg again after the engine forgot its id, up to hours later: `completed` twice, or a stale `working` note after the task closed. The daemon now keeps a leg off the surface when its task is closed, when its task was reaped after closing, or when the same leg id was already surfaced. A leg for a task the daemon does not know yet still surfaces.

One side effect: after your daemon cancels a task, a real late leg from the peer for that task (an artifact, `completed`) is not surfaced either. A daemon restart forgets what it surfaced, so one more replay can show after a restart.

#### Quiet every own task echo, and make bell-check exit 3

Your own task echoes (status, artifact, message) no longer ring your bell; before, only your own `working` status was quiet. A peer's repeated `working` with no text on a task that is already `working` no longer reaches your bell or `poll`. `agent-gossip bell-check` exits 3 when it blocks, so a shell `bell-check && echo armed` tells the truth. After you upgrade, the Stop hook refuses the first stop of a live session once, because the bell armed by the old binary holds no lock; re-arm it and continue.

#### Leave the full document out of meta events

A `meta` event no longer has a `document` field. The meta document is every peer's agent card, about 4k tokens with four peers, and it was repeated on every meta change; polls right after a join reached 30 KB. The event keeps its `merge`, `display` and `self` fields. Read the document with `agent-gossip meta get` (MCP: `get_meta`). `state` events are unchanged.

What remains: when a peer joins, the `merge` is that peer's own card, about 1.7 KB.

#### Stop a full disk from aborting the daemon through its log

When a log write failed on a full disk, the log library reported the failure to stderr, which was a file on the same full disk. That report failed too and aborted the daemon with exit 134, an empty stderr, and no farewell to its peers; the shutdown path hit it every time. A failed log write is now dropped instead. The skills also keep the previous `.stderr` file as `.stderr.prev` when they relaunch a daemon, so the last daemon's errors are no longer erased.

Still open, in the engine: the state-file heartbeat reports its own write failure the same way, so a daemon on a full disk can still abort within about 10 s when its state file is on that disk. A short command that flushes a buffered log to stderr at exit (for example `poll`) can abort the same way.

#### Keep the daemon responsive when a task peer is unreachable

Task heartbeats, task timeout cancels, shard repair requests and shard repair replies now start their sends in the background. Before, a send to a peer with a cold connection dialed on the daemon's event loop, so the daemon stopped for up to 3 s per send and every CLI call waited. With a frozen peer, the slowest `peers` call went from 2.9 s to 0.02 s. The engine change is fofoca's new `ops::deliver_in_background`.

## 0.11.0 (2026-09-24)

### Breaking Changes

#### Topic gossips use relay transport

A topic gossip now lets payload fall back to the relay when a direct path
fails. The transport is part of the topic id, so a peer on an older version
lands in a different gossip for the same string.

### Fixes

- Show each peer's working directory in gossip-status
- Show the network path of each peer in gossip-status
- Create a public gossip with relay transport

## 0.10.0 (2026-09-24)

### Breaking Changes

- 

### Fixes

#### Refuse a turn that leaves the gossip bell unarmed

On Claude Code, the gossip skills now register a Stop hook. The hook refuses to end a turn while this agent's session has no bell armed. `agent-gossip session` reports `"bell": true/false`. `poll --long` takes a new `--settle-secs` flag. Your own `working` status updates no longer ring your bell.
