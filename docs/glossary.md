# Concept Glossary

> 🚧 **Under construction.** This document is a work in progress and may be
> incomplete or out of date.

One concept, one word. The codebase is organized in layers, and each layer
owns a term it never lends to another. Keeping these distinct is what stops a
transport detail from leaking into membership prose, or a display label from
being mistaken for an identity. When reading or changing code, hold the
following meanings.

For the mechanisms behind these terms, see the companion docs:
[swarm-hash.md](./swarm-hash.md) (the `💬…` token),
[discovery.md](./discovery.md) (rendezvous, beacon, lookups, directories),
[gossip.md](./gossip.md) (message fan-out),
[topologies.md](./topologies.md) (network shapes), and
[history-integrity.md](./history-integrity.md) (signing and fork detection).

## Terms

### endpoint / link

*Layer: transport · keyed by node id (hex).*

An iroh `EndpointId` and the gossip neighbor link to it. This is pure
plumbing — the node id itself is never surfaced to operators or agents. The
one thing derived from it is the per-participant **connected vs gossip** tag
(`agent-gossip peers` / `swarm_info` `reach`): `participant_endpoints` maps a nickname
to its self-advertised endpoint, so the roster can mark a peer as a live link
or a relayed one — a boolean, never the node id.

State: `linked_endpoints` (the links), `participant_endpoints` (the bridge).

### unicast

*Layer: transport · keyed by node id (hex).*

The point-to-point QUIC channel a **directed** frame (one addressee) takes when
its addressee is dialable — a real client/server link this node opens to one
participant's endpoint, on its own ALPN (`agent-gossip/unicast/1`), off
the gossip flood. Gossip stays the transport for broadcasts and the fallback
for a directed frame whose addressee can't be reached by unicast. Without it,
a directed frame (`a2a_req`/`a2a_resp`, a task push leg, a `pong`) floods every
neighbor and is filtered at the receiver — O(N) fan-out to reach one peer.

Distinct from **link** (a gossip active-view neighbor) and from the roster's
**connected/gossip** `reach` tag, which stays a gossip-overlay fact — a live
unicast connection does **not** make a peer show as `connected`. Also distinct
from the **a2a** JSON-RPC binding. Inbound unicast frames are validated +
dispatched by the *same* `gossip::ingest` path as gossip, so signature-verify
and dedup are identical and a frame delivered over both transports surfaces
exactly once. Every wire frame stays ≤ `MAX_MESSAGE_SIZE` on both planes, so
any frame remains gossip-carriable and anti-entropy-healable.

State: `unicast_pool` (the per-peer connection pool). See [`src/unicast`].

### whisper

*Layer: transport · keyed by node id (hex).*

The directed, private counterpart of broadcast **gossip** — a *whisper* passed
quietly ear-to-ear along a chain (cf. Ethereum's Whisper). The multi-hop
transport a **directed** frame takes when its addressee is *known* but not
**directly** reachable by **unicast**: the initiator source-routes a **circuit**
— a telescoping chain of QUIC connections through **peers it is already connected
to** — over its own ALPN (`agent-gossip/whisper/1`). Each hop peels one
**onion**-sealed layer (reusing **seal**), learning only its successor, and
splices the payload straight through; the terminal hop delivers into the *same*
`gossip::ingest` seam as unicast, so the addressee ingests a whispered frame
identically. Forwarding peers (**whisperers**) need **not** be publicly reachable
— this is the serverless, multi-hop counterpart to iroh's own (server-based,
single-hop) **relay**, and deliberately named apart from it.

Route selection is **proactive link-state**: every node gossips its own measured
**link-vector** (a `linkstate` frame: its neighbours + per-link `LinkMetric` + its
X25519 key), so each node holds the whole metric-weighted mesh **graph** and
computes routes locally with Dijkstra — including up to N **node-disjoint**
alternates tried best-first before falling back to gossip. Tier order for a
directed frame: direct **unicast** → **whisper** → gossip. Gated by
`--no-whisper`. State: `link_state` (`LinkStateStore`). See [`src/whisper`].

### participant

*Layer: membership · keyed by nickname.*

A member of the swarm other than yourself — the roster. The live count
includes you: `participant_count == participants.len() + 1`, where the `+1`
is self.

State: `participants`.

### peer

*Layer: prose only.*

An informal synonym for "another participant". Fine in comments and
conversation, but never a load-bearing identifier or field name in new code —
reach for **participant** instead.

### swarm hash

*Layer: identity · keyed by seed.*

The `💬…` id: a self-describing token carrying the `seed`, the name, and the
swarm's **config** (lookups). The config is mixed into the gossip topic, so
every member necessarily shares it, and `join` needs nothing beyond the hash
itself.

Code: `protocol::swarm` (`Swarm` / `SwarmConfig`). Byte layout:
[swarm-hash.md](./swarm-hash.md).

### topic swarm

*Layer: identity · keyed by seed.*

A `Swarm` whose `seed` is derived from an arbitrary string —
`SHA256(TOPIC_DOMAIN ‖ trim(string))` — rather than minted randomly at
`create`. The name is the string itself sanitized into a `SwarmName` (leading
URL scheme dropped — plus the `?query`/`#fragment` for an http(s) URL — invalid
runs → `-`, `/` and URL chars kept, capped at 32 with a trailing `…`, or `topic`
if empty; this affects the name only, not the seed), and the config is always
the public preset — so the
**string alone** determines the swarm: anyone running `agent-gossip topic <string>`
converges. Joined via the `topic` command, not `join`.

Code: `protocol::crypto::topic_seed`, `Swarm::from_topic`,
`SwarmName::from_topic_string`. See [discovery.md](./discovery.md) §7.

### password

*Layer: identity · optional, per swarm or per transfer ticket.*

An optional knowledge factor on top of the bearer capability: with one set,
holding the `💬…` hash or ticket alone no longer admits. The password's value
never travels. For a **swarm**, `create --password` stretches it with Argon2id
(salt = the seed) into a key that replaces the seed in *every* derivation
(topic, rendezvous, port ladder), and the hash carries a one-way **verifier**
of that key so `join` can check a candidate locally — a wrong password fails
immediately, before any network. For a **ticket** (pipe/port/file), the
consumer presents the Argon2id stretch of the password (salt = the ticket
secret) instead of the raw secret; the producer verifies online and rejects
with a distinct "wrong password" close. Tickets carry no verifier —
advertised ads are public, and a verifier there would be an offline grinding
target; the swarm hash accepts that trade for local verifiability. A
passworded swarm or ticket is therefore safe to **advertise**.

Code: `protocol::crypto` (`stretch_swarm_password`, `password_verifier`,
`TicketAuth`), `Swarm::{set_password, apply_password}`.

### rendezvous

*Layer: identity · keyed by seed.*

The seed-derived bootstrap identity — a keypair plus ports — that every joiner
computes locally from the hash. No peer address is ever stored; the rendezvous
is recomputed, not shared.

Code: `protocol::crypto`.

### identity key

*Layer: identity · keyed by pubkey.*

A per-participant Ed25519 keypair minted in-process at `create` / `join`
(ephemeral). The **public key is the author's identity**; the nickname is only
a non-unique display label and is never claimed cryptographically. Every
message is signed with this key and verified on receipt, and fork detection
keys on it. It is distinct from the shared
**rendezvous** key and from the transport **endpoint** key.

Code: `protocol::identity`. Design: [history-integrity.md](./history-integrity.md).

### fork

*Layer: integrity · keyed by pubkey.*

Equivocation: one **identity key** signing two different messages at the same
`seq`. The swarm detects this — it never prevents or auto-resolves it — and
surfaces it once per key as a `fork` event. Both conflicting messages are
kept; resolution is left to the operator.

Code: `state::note_msg_seq`.

### beacon

*Layer: role.*

The single live member currently binding and serving the rendezvous endpoint.
The role migrates to a surviving member when the holder dies.

Code: `beacon`.

### lookup

*Layer: lookup.*

A mechanism that resolves a seed-derived `rendezvous_id` into a reachable
address: mDNS (LAN), the mainline DHT, or the relay — the `--mdns / --dht /
--relay` allowlist. Each lookup is **feature-complete on its own**; the others
are reliability layers, not dependencies.

Code: `lookup`.

### ladder

*Layer: transport.*

An *ordered* set of rendezvous rungs the beacon claims in preference order, so
every member converges on the same one. There are two instances: the
seed-derived **loopback-port** ladder (private) and the **relay** ladder
(public — the n0 prod set, or a custom `--relay a,b,c`). The beacon homes on
the first reachable or free rung and re-elects there on death.

Code: `beacon` (ports), `lookup::select_bootstrap_rung` (relays).

### surfaced

*Layer: presentation · keyed by nickname.*

A participant whose arrival was actually *shown* to the operator or agent.
`surfaced ⊆ participants`; it is presentation-only, so the roster stays
complete for anti-entropy regardless of what has been displayed.

State: `surfaced`.

### quiet

*Layer: heartbeat · keyed by nickname.*

A participant evicted for going silent past `ALIVE_TIMEOUT_SECS`, but who may
yet return.

State: `quiet`.

### directory

*Layer: discovery · keyed by directory name.*

A named, well-known public `Swarm` (`derive_secret(DIRECTORY_BASE_SEED,
name)`) that swarms **advertise** their `💬…` id into and that **discover**
browses. It is not a server — it is itself a swarm, with its own rendezvous,
reached via the lookups. The default directory is `global`.

Code: `directory`.

### advertise

*Layer: discovery.*

A `create`-time opt-in (`--advertise[=<directory>]`) that re-broadcasts this
swarm's own id into a directory so `discover` can find it. It is create-only,
and broadcasting the id makes the swarm open to anyone who finds it.

### discover

*Layer: discovery.*

Browse a directory's live swarms (`agent-gossip discover`) and join one — the consumer
side of **advertise**.

### task

*Layer: a2a · keyed by `task_id` (correlation) + the two parties' nicknames.*

The delegation **primitive**, native A2A on the wire: a directed `SendMessage`
with no `taskId` (over the gossip request/response binding) creates it — the
**worker** (the A2A server) **mints the task id** and returns the `Task`.
`TaskStatusUpdate`s and `TaskArtifactUpdate`s (worker→initiator push) and
mid-task `SendMessage`s (initiator→worker follow-ups) advance it, and the
**worker** authors the terminal `completed` after the initiator's approval
message — native server-completes semantics (see
[a2a-binding.md](./a2a-binding.md)). The daemon state machine (`a2a::task`)
owns the *coarse* lifecycle (state advance, the per-task idle-debounce timeout,
the ball-owner keepalive, the 100-content-message cap); the *content* is owned
by the skill. A worker-push leg (`a2a_status`/`a2a_artifact`) is delivered to
all members for relay but **surfaced and logged only by its addressee and the
sender's own echo** — a third party never sees it; a beat is liveness plumbing
(never logged). Task legs are not part of the per-author hash chain or DAG
(presence-like). There is **no** wire behavior discriminator: the two delegation
UX flows below distinguish themselves by how the skill uses the task, not a
marker.

Two skills ride this primitive. `/gossip:task` is the **report-back** flow — the
worker returns a result (`artifact`), the initiator approves, and the worker
completes; it creates one or more independent tasks (each its own `task_id`,
worker, and completion criteria) and surfaces each result as it returns, with no
group-level outcome. `/gossip:handover` is the **walk-away** flow (see below).

**Keepalive vs. liveness.** While the ball-owner is silent, its daemon emits a
`working` keepalive beat so a genuinely-working owner is not falsely timed out.
But the keepalive is bounded by **skill** liveness, not process liveness: it
only fires while a real leg has been driven within `TASK_KEEPALIVE_MAX_SECS` (a
leg the daemon's own keepalive never counts as). Past that, the keepalive stops
and the peer's debounce reaps the task — so a crashed or abandoned skill cannot
hold the peer forever.

Code: `MessageKind::{A2aReq,A2aResp,A2aStatus,A2aArtifact}`,
`gossip::recv::ingest_remote_message`, `gossip::emit_task_status`/`emit_task_artifact`,
`a2a::task` (`TaskRecord::should_keepalive`, `adopt_initiator`).

### handover

*Layer: skill behavior on top of **task**.*

A UX behavior on the task primitive, driven entirely by the `/gossip:handover`
skill: delegate a task/plan and walk away. The handoff completes the moment the
worker **accepts** (`state:"working"`); the worker then runs the work **itself**
and completes on its own — no result flows back (the difference from
`/gossip:task`, which returns a result the initiator approves). Because the wire
has no behavior discriminator, the "walk-away vs report-back" intent lives in how
the skill uses the task (and the brief's phrasing), not as a wire field. Adds no
wire type of its own.

### a2a

*Layer: protocol — the agent-communication layer.*

The [A2A protocol](https://a2a-protocol.org): every semantic exchange between
participants — chat, delegation, task status, results — is an A2A object
(`Message`, `Task`, status/artifact update) from `src/a2a`. The layer owns the
A2A spec's words — *message parts*, *artifact*, *role*, *context* — and is
carried by a **binding**; everything below it (signing, dedup, digests,
presence, shards) is replication machinery, not communication. Note the word
`Part` belongs to this layer (a message's content unit); the transport slice
formerly called "part" is a **shard**.

### binding

*Layer: protocol.*

One concrete carrier of the A2A core: the **gossip binding** (custom, spec
§12 — always on, the peer-to-peer plane) or the **local JSON-RPC binding**
(`--a2a-serve`, off by default — how off-the-shelf A2A clients on this
machine reach the swarm). Both execute the same operations against the same
state; the JSON-RPC binding relays writes onto the gossip binding. The
gossip binding additionally carries a **request/response** mode (`agent-gossip a2a
call`): a peer calls another peer's A2A server and awaits its reply over
gossip (a safe method subset — reads, a party-checked cancel, and
SendMessage directed at the peer). See [a2a-binding.md](./a2a-binding.md).

### frame

*Layer: transport — the `Message` struct.*

The signed wire envelope (protocol version `3.0`): id, kind, swarm, author,
timestamp, body, signature, history-integrity fields, shard header. The
gossip binding's transport layer, below A2A — a frame carries exactly one
A2A-domain payload in `body` (chat/status/artifact) or a plumbing body
(presence, digests, ping/pong, state events); it never grows a competing
message vocabulary. Receive-side, every logical frame passes the A2A
boundary gate (`a2a::gossip`) — a payload that fails to parse or
contradicts its frame is dropped whole.

### card

*Layer: a2a · keyed by nickname.*

A participant's `AgentCard` — its canonical A2A self-description (A2A v1.0:
`supportedInterfaces[]` each with a `protocolVersion`, capabilities, declared
extensions, default skills, and its Ed25519 identity carried in the gossip
`AgentInterface` url, `swarm+gossip://<pubkey>`). Each member's daemon publishes its card
into the **meta** channel at `/peers/<nick>/card` on join — the one channel
write the binary itself makes (see the amended invariant under *shared
state*) — so peers enumerate each other's cards from the meta document with
no HTTP anywhere. Read with `agent-gossip card [--peer <nick>]`. Agent-side facts
the daemon cannot know (`model`, `harness`, `host`, extra skills) remain the
agent's own merge, as sibling keys under `/peers/<nick>`.

The receive path **enforces** that a member only writes its own
`/peers/<self>/card`: a meta merge touching another peer's `card` (set or
`null`-delete) is dropped before it folds, so the card — and the identity in
its gossip interface url — is a contract, not a forgeable label
(`state_doc::meta_merge_forges_foreign_card`).

Code: `a2a::card`.

### shard

*Layer: protocol — a header on **message**.*

One slice of a body too large for a single gossip message. When a body
exceeds `MAX_MESSAGE_SIZE`, the sender splits it into several ordinary signed
messages, each carrying a `shard` header — a `group` (a UUID shared by the
body's shards), an `idx`, and the `total` count. Each shard is a real message
(own id/seq/signature); shards of a small group (`total <=`
`LOGGED_SHARD_GROUP_MAX_TOTAL`) are retained in the **message log**, so a
missing one heals through anti-entropy like any message, while a bigger
group's shards skip the log on both ends (one huge body must not evict the
anti-entropy history) and heal through **shard repair**: the sender caches
its outbound frames and a receiver whose group stalls asks it — the
`shard/repair` gossip-RPC method — to re-send the missing indexes. The
receiver buffers shards in the
dedicated **reassembly store** — bounded by byte budgets (per group, per
author, global) and a stale-group TTL, never by a shard count — and keyed
also by author key, so a crafted cross-author shard can't inject a slice.
The reassembled logical message is the only thing surfaced; the raw shards
never surface. The one send-side limit is `MAX_LOGICAL_BODY_BYTES` (a local
input ceiling — bigger payloads belong on the **blob** channel). The split is
invisible to agents: a body sends and arrives whole, on any transport.
(Renamed from *part*: the A2A layer owns that word for a message's content
unit.)

### seal

*Layer: protocol — an end-to-end encryption of a **frame** body, under the
**a2a** layer.*

The encryption applied to a **directed** frame (one addressed with a `to`:
`A2aStatus` / `A2aArtifact` / `A2aReq` / `A2aResp`) so only the addressee can read
it. A NaCl-style sealed box (`src/protocol/seal.rs`): a fresh ephemeral X25519
key does ECDH with the recipient's static X25519 public key (published in the
recipient's card under the `swarm-seal` extension, derived from its identity
seed), the shared secret is run through `derive_secret` into a ChaCha20-Poly1305
key, and the body is encrypted and Base58-wrapped into a `MessageBody`-safe JSON
envelope. The recipient decrypts with its static secret; a relay forwards the
frame and **verifies the Ed25519 signature** (which covers the ciphertext) but
cannot read the body. Only the body is sealed — routing metadata (`to`,
`task_id`, author, kind) stays cleartext so relays route and anti-entropy heals.
**Broadcast (`A2aMsg`) and the `state`/`meta` channels are never sealed** — their
audience is every member; they stay public and signed. Forward secrecy comes
from the per-frame ephemeral key; sender authenticity from the frame signature.

### blob

*Layer: blob transport — a direct QUIC channel beside gossip, under the **a2a**
layer.*

A large file carried by an A2A **part** without inlining its bytes over gossip.
The producer's daemon serves the content — addressed by its SHA-256 — from a
per-peer spool (`<RUNTIME_DIR>/<swarm-prefix>/<nick>.blobs/<hash>`, hardlinked or
copied from the source so the original can change freely) over a dedicated,
lazily-bound endpoint on the `agent-gossip/blob/1` ALPN. The **blob
reference** — a `💬…` Base58Check *ticket* carrying the producer's address, a
bearer secret, the hash, and the size — rides gossip inside a `Part.url`. The
ticket shares the swarm's `💬` brand with the swarm id and the a2a bridge
ticket; a *kind* byte inside the framed payload tells the three apart, so a
wrong-kind token fails cleanly on decode. The consumer decodes it, dials the
producer, presents the secret, and streams the bytes to disk, verifying the
SHA-256 as they arrive (`agent-gossip a2a fetch` — by default into the session's
`<nick>.recv/` folder, or to stdout with `--output -`). Symmetric: an input
file rides a request `Message.parts`, an output rides a result `Artifact.parts`.
Confidentiality equals swarm membership (the flooded ticket lets any member
fetch); availability lasts only while the producer's daemon is alive.

### shared state

*Layer: state · two **channels** per swarm (`state`, `meta`), each a document
derived from its own **state log**.*

A JSON document the whole swarm shares, separate from the chat message log. It
is an **automerge CRDT**: each member holds a replica, and members exchange
signed **changes** that automerge merges conflict-free, so the same change set ⇒
byte-identical document on every member (see the *Shared state converges
deterministically* invariant). It is never sent whole on the wire; only changes
are (`agent-gossip state get` reads the local replica as JSON).

Each swarm carries **two channels**, `state` and `meta` — the same machinery
(the [`SwarmDoc`](#state-doc) engine), differing by **convention** and one gate:
`state` is the task working area; `meta` holds swarm metadata, by convention
`/peers/<nick> = { model, harness, host }` that each agent self-reports (`host`
is the machine's self-reported hostname). `meta` alone gates **card forgery**
(see [state doc](#state-doc)) and seeds a deterministic `/peers` container so
concurrent per-peer writes merge. With exactly one exception the binary never
writes a channel itself: the daemon publishes its own **card** at meta
`/peers/<nick>/card` on join (architectural peer self-description, not app
state). Every other change is `agent-gossip state merge` / `agent-gossip meta merge`. A change
surfaces as the `state` / `meta` event, carrying both the merge and the
newly-derived document.

Code: `daemon::doc::SwarmDoc`, `protocol::Channel`, `OutputEvent::StateChanged`.

### state doc

*Layer: state · `MessageKind::State` / `MessageKind::Meta`, one automerge doc +
signed-frame store per **channel**.*

The convergent document engine ([`SwarmDoc`](#shared-state)): an automerge
document plus a `HashMap<ChangeHash, Message>` of the signed frames that carried
each applied change — the **re-serve store** (a peer forwards another author's
change with its original signature intact). Distinct from the chat **message
log**: un-pruned (verifiable history = full history; compaction is deferred, as
before), and reconciled by a **heads-based** anti-entropy digest — a peer
advertises its automerge heads and a holder computes exactly the changes it
lacks (`changes_since`), so a late joiner backfills the whole history over
successive rounds with no windowing. Changes apply in causal order (orphans
buffer until deps land) and, on `meta`, pass a **card-forgery gate**: a change is
rejected (never merged) if it would alter any peer's `/peers/<nick>/card` other
than the author's own — the card carries that peer's cryptographic identity.
Every honest member runs the same gate, so a forgery converges nowhere.

Code: `daemon::doc::SwarmDoc`, `gossip::antientropy::{broadcast,handle}_state_digest`.

### change (state merge)

*Layer: state · one automerge change, composed from an RFC 7386-style merge in a
`State`/`Meta` event body.*

One modification to the **shared state**. The `agent-gossip state|meta merge` surface
still takes an RFC 7386-style merge document (an object deep-merges — each key
set, a `null` value deletes, nested objects recurse, arrays replace wholesale),
which is translated into a single automerge change. Two semantics differ from a
plain JSON fold: (1) a **non-object top-level merge is rejected** — automerge's
document root is always a map, so there is no "replace the whole document" case;
(2) concurrent edits merge **conflict-free** via the CRDT rather than by a
deterministic replay order. Each writer still touches only its own subtree, so
concurrent writers to different keys never clobber. Every change is carried in a
signed frame; the signature covers the change.

Code: `daemon::doc::SwarmDoc::{build_change, ingest}`, `daemon::state_doc::change_body`.

## Layering

Don't conflate the three: **rendezvous** / **beacon** bootstrap a swarm you
*already hold*; a **directory** finds swarms you *don't* — and is itself a
swarm with its own rendezvous, reached via **lookups**. Three distinct layers.

## Invariants

These follow from the layering above.

### Join horizon

A message is surfaced if and only if its `timestamp >= joined_at`. There is
one cutoff, computed once (`lifecycle::observe`) and applied uniformly to
every surfaced event. A node still relays and logs pre-join traffic for
anti-entropy; it simply never *shows* it.

### Lifecycle is one vocabulary

Arrival and departure surface exactly once each, as nickname-keyed membership
presence (`joined` / `left`), plus the heartbeat events `peer_timeout` /
`peer_return`. All are join-horizon gated and symmetric — a departure is
surfaced only if the matching arrival was. There is **no** transport-level
`peer_join` / `peer_leave` event: a raw link to an opaque node id is not
participant lifecycle. (`agent-gossip leave` is a CLI verb on top of this
vocabulary, not a new event: it stops a local daemon, whose shutdown emits
the one `left`.)

### author

The `Nickname` that wrote a message. It is the same value-type as a
participant id; the distinct word marks "sender of *this* message", not a
separate concept.

### Shared state converges deterministically

Every member's **shared state** is an automerge CRDT replica fed the same set of
signed **changes**; automerge merges them conflict-free, so the document is a
pure function of the *set* of changes — the same set always yields the
byte-identical document, regardless of arrival order. Changes apply in causal
order (a change whose dependencies have not yet arrived buffers until they do),
and convergence is unconditional.

*Concurrent same-field edits* resolve by automerge's own deterministic rule (an
actor-id tiebreak) — convergent, but the value a reader sees may not be the one a
given writer intended. In practice writers stay in their own subtree
(`/peers/<nick>/…`), so this only arises for genuinely co-edited fields; turn-based
use (a change per member on its turn) avoids it, and multi-key updates that must
land together go in **one** merge object. The one write automerge does *not*
merge is a per-peer **card** on `meta`: a change altering another peer's
`/peers/<nick>/card` is rejected before it merges (that card is cryptographic
identity), so a forgery converges nowhere.

### Lookups are independently sufficient

Each lookup (mDNS, DHT, or relay) is **feature-complete on its own** — any
single one enabled must both bootstrap *and* run a swarm with no other
present. Additional mechanisms are **reliability layers**, never feature
dependencies; they widen reachability and remove single points of failure.

This is why the beacon homes on **one deterministic relay rung** rather than
spreading across the set: iroh does not reliably race multiple relay
candidates in an `EndpointAddr`, so relay-only bootstrap needs a rung every
member computes identically — the **ladder**. Under equal relay visibility,
"first reachable rung" is a global function, so all members meet at the same
rung and fail over together. Under *unequal* visibility the relay layer can't
guarantee a meeting, and that is exactly where mDNS and the DHT take over.
(Participant *connectivity* still uses the full multi-relay set for
resilience — only the rendezvous rung is pinned.)
