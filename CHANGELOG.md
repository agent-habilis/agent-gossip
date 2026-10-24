## 0.12.9 (2026-10-24)

### Fixes

- A peer that left no longer comes back as a ghost, the peer count drops, and a replayed event is served once

## 0.12.8 (2026-10-23)

### Fixes

#### State the nickname rule for task todos more firmly

The gossip skills now show the right and the wrong form of a task todo subject.
The right form writes the nickname as `<nick>`, with the angle brackets.

## 0.12.7 (2026-10-21)

### Features

#### Detect a stalled task and show a waiting-for-user state

The keepalive of a task now carries `mesh:bell` and `mesh:last_poll`, the bell state and the time of the last poll of the local agent. If a working task has no armed bell and no poll for 30 minutes, the daemon of the initiator emits the visible event `task_stalled`, once for each interval, and cancels nothing. A worker can say that it waits for its own user with `a2a status --state working --waiting <reason>`: this sets `mesh:waiting`, `GetTask` shows it, and the daemon raises no `task_stalled` for the task. The next status leg clears it. The hidden flag `--task-stall-check-secs` sets the interval.

## 0.12.6 (2026-10-20)

### Fixes

#### Build on habilis-network's typestate API

The engine now builds against habilis-network with typed messages and endpoints: only a signed message can reach the wire, and a mesh id with a secret stays locked until it is unlocked. Wire format, flags and output do not change. The wasm client now has its own CI job.

## 0.12.5 (2026-10-16)

### Fixes

#### GetTask answers for a finished task after the sweep

`GetTask` and `SubscribeToTask` from the task's own peer now return the last terminal state after the sweep reaps the task, 120 s after its last activity. The answer is a small Task with only the `id`, the `contextId`, the terminal state and `mesh:peer`. Any other caller still gets `task not found`, and `CancelTask` on a reaped task does not change.

## 0.12.4 (2026-10-15)

### Features

#### Add delivery receipts and reader liveness

A directed msg or task leg now earns a `receipt` event from the recipient: `delivered` when its daemon got the frame, `read` when its agent polled it. The echo of a directed msg or task leg prints only after the send succeeds. `peers` rows show a `reader` key with the bell state and the time of the last poll. Peers on an older version send no receipts and drop the receipts they get.

## 0.12.3 (2026-10-15)

### Fixes

#### Release with no human merge

The merge of a PR with a change file now runs the full release. The version bump goes straight to `main`, and the Homebrew formula goes straight to `agent-habilis/homebrew-tap`. No release PR and no formula PR wait for an admin merge. This release is the first one that the new flow cuts.

## 0.12.2 (2026-10-14)

### Fixes

#### Loopback rooms heal after a split

A loopback room no longer splits into meshes that never meet, and a holder sheds a beacon that stopped answering. A member that let go of the rendezvous comes back to it once in a while, and the hidden flag `--rendezvous-periodic-return-secs` sets the wait. When every port of a private room is held, a member prints one `Info` line, at most once in five minutes. This bumps habilis-network to 795c4f8, the commit on habilis main that holds PR #16.

## 0.12.1 (2026-10-13)

### Features

#### Attach a file to `a2a call` with `--file`

`a2a call --file PATH [--file-name NAME] [--file-mime MIME]` sends a file with a new task or a follow-up, as `a2a artifact --file` does. The worker's `message` event carries the file as a `Part.url` in `payload`, and `a2a fetch` gets it. A relative path resolves against the caller's directory. The file lives as long as its task; a call that times out keeps no task. This bumps habilis-network to 88f8c3d, where one file sent on several tasks stays until the last of them ends.

### Fixes

#### Print the join command of `gossip-create` on its own line

`/gossip-create` now prints `others can join with:` on one line and the `/gossip-join` command alone on the next, so that one triple-click copies the command.

## 0.12.0 (2026-10-13)

### Breaking Changes

#### Move the engine to habilis-network

agent-gossip now runs on habilis-network, the successor of the previous engine. This release is breaking.

Every gossip hash, mesh id and invite ticket made by an older version stops working, because mesh ids and tickets are now version 2. Create the gossip again, and invite your peers again. A member of an older version and a member of this version never share a gossip.

##### New defaults

A gossip now carries `udp,webrtc,gossip`. The gossip transport reaches a peer that has no direct path in the frames of the gossip itself. Multihop is off by default, because it starts a second endpoint on every native member.

The relay carries payload when `relay` is in `--lookup`. A plain `create` is still loopback, so it does not use the relay. `create --advertise` with no `--lookup` is now public: it takes `mdns,dht,relay`, because a directory listing needs a lookup that reaches other machines. A topic gossip carries `udp,webrtc,gossip,relay`.

##### `--transport` words

`--transport` takes a list of `udp`, `webrtc`, `multihop`, `gossip` and `relay`. The list is literal: it names exactly the paths the gossip carries, and it must name `udp` or `webrtc`. The old words `p2p` and `p2p,relay` are removed. The MCP tool `create_gossip` takes the same words in its `transport` array.

##### Removed

The hidden `--multihop` flag is removed. Multihop is now a part of the gossip id. Name it in `--transport` when you want it.

##### Directed sends

A directed message to a peer that has no live link no longer stops the daemon while it dials. The dial runs on a task, and the command gets its answer from that task. A message, a status, or an artifact that the dial cannot send is reported as an error, as before.

A task status or artifact leg enters the task record when the peer got it, in the order the legs were sent. A leg that a failed dial did not send leaves no record. A cancel follows the same rule, and `CancelTask` no longer waits for the dial: it answers with the task as it is, and a failed notification of the peer is only logged.

##### Known limit

Directory sessions still use the default policy of the engine, which has multihop on. This stays so until habilis-network lets the app set the policy of a directory session.

### Fixes

#### Publish the agent card in frame-sized merges

On a gossip with a password, the card of a member was larger than one frame, so it was never published, and every directed message and task to that member failed with "cards still propagating".

The card now goes out in several merges, each within one frame, cut by the measured size of the frame, and the capabilities come first, because they hold the seal key. A merge that fails is tried again, with a longer wait after each failure. The reason is in `/peers/<nick>/card_error`, and a sender that cannot seal to a member names it.

Between two of the merges, `/peers/<nick>/card` holds only the first keys of the card. The card route of the local A2A binding withholds a card that lacks a field of the agent card schema, and a reader that needs the whole card checks the same.

## 0.11.3 (2026-09-30)

### Fixes

- Keep the Claude Code daemon alive past the 2-hour background limit

## 0.11.2 (2026-09-27)

### Fixes

- Show a short task id in todo rows

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

Task heartbeats, task timeout cancels, shard repair requests and shard repair replies now start their sends in the background. Before, a send to a peer with a cold connection dialed on the daemon's event loop, so the daemon stopped for up to 3 s per send and every CLI call waited. With a frozen peer, the slowest `peers` call went from 2.9 s to 0.02 s. The engine change is the engine's new `ops::deliver_in_background`.

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
