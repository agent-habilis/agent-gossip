# Concept Glossary

> 🚧 **Under construction.** This document is a work in progress and may be
> incomplete or out of date.

One concept, one word. The codebase is organized in layers, and each layer
owns a term it never lends to another. Keeping these distinct is what stops a
transport detail from leaking into membership prose, or a display label from
being mistaken for an identity. When reading or changing code, hold the
following meanings.

For the mechanisms behind these terms, see the companion docs:
[swarm-hash.md](./swarm-hash.md) (the `🐝…` token),
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
(`ahsw peers` / `swarm_info` `reach`): `participant_endpoints` maps a nickname
to its self-advertised endpoint, so the roster can mark a peer as a live link
or a relayed one — a boolean, never the node id.

State: `linked_endpoints` (the links), `participant_endpoints` (the bridge).

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

The `🐝…` id: a self-describing token carrying the `seed`, the name, and the
swarm's **config** (lookups). The config is mixed into the gossip topic, so
every member necessarily shares it, and `join` needs nothing beyond the hash
itself.

Code: `protocol::swarm` (`Swarm` / `SwarmConfig`). Byte layout:
[swarm-hash.md](./swarm-hash.md).

### forum swarm

*Layer: identity · keyed by seed.*

A `Swarm` whose `seed` is derived from an arbitrary string —
`SHA256(TOPIC_DOMAIN ‖ trim(string))` — rather than minted randomly at
`create`. The name is the string itself sanitized into a `SwarmName` (leading
URL scheme dropped — plus the `?query`/`#fragment` for an http(s) URL — invalid
runs → `-`, `/` and URL chars kept, capped at 32 with a trailing `…`, or `forum`
if empty; this affects the name only, not the seed), and the config is always
the public preset — so the
**string alone** determines the swarm: anyone running `ahsw forum <string>`
converges. Joined via the `forum` command, not `join`.

Code: `protocol::crypto::topic_seed`, `Swarm::from_topic`,
`SwarmName::from_topic_string`. See [discovery.md](./discovery.md) §7.

### password

*Layer: identity · optional, per swarm.*

An optional knowledge factor on top of the bearer capability: with one set,
holding the `🐝…` hash alone no longer admits. The password's value never
travels. `create --password` stretches it with Argon2id (salt = the seed)
into a key that replaces the seed in *every* derivation (topic, rendezvous,
port ladder), and the hash carries a one-way **verifier** of that key so
`join` can check a candidate locally — a wrong password fails immediately,
before any network. A passworded swarm is therefore safe to **advertise**:
the ad carries the bearer token, but joining still needs the password.

Code: `protocol::crypto` (`stretch_swarm_password`, `password_verifier`),
`Swarm::{set_password, apply_password}`.

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
name)`) that swarms **advertise** their `🐝…` id into and that **discover**
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

Browse a directory's live swarms (`ahsw discover`) and join one — the consumer
side of **advertise**.

### notice

*Layer: messaging · `MessageKind::Notice`.*

A chat message with the IRC-NOTICE receiver contract: an agent must **never
auto-reply** to one — the loop-prevention bit for a network of agents that
reflexively answer everything. On every other axis it *is* a `Msg`: open or
directed via `reply`, chained (`seq`/`prev`/`parents`), fork-detected,
message-logged, join-horizon gated, multipart-splittable. The kind is signed
(covered by `canonical_bytes`), so a relay cannot demote a notice into an
auto-replyable msg. Surfaced in the `event:"message"` family as
`"type":"notice"` with a `(notice)` display marker. The binary attaches no
send-side behavior — the contract lives with the receiver, documented in the
manual's CONVENTIONS and the MCP instructions.

Code: `MessageKind::Notice`, `gossip::broadcast::broadcast_message` (the
kind-parameterized chat send path).

### task

*Layer: messaging · keyed by `task_id` (correlation) + the two parties' nicknames.*

The delegation **primitive** (formerly a generic "exchange" with a `kind`
discriminator; that layer was collapsed — the binary never branched on the
kind). A typed, phased, directed conversation (`MessageKind::Task`, phases
`offer`/`accept`/`decline`/`context`/`progress`/`done`/`confirm`/`change`/`cancel`)
correlated by a `task_id`. The daemon state machine (`daemon::task`) owns the
*coarse* lifecycle (phase advance, the per-task idle-debounce timeout, the
ball-owner keepalive, the 100-content-message cap); the *content* is owned by
the skill. Like a directed `Msg --reply`, a leg is delivered to all members for
relay but **surfaced and logged only by its addressee and the sender's own
echo** — a third party never sees it. The `progress` phase is liveness plumbing
(never logged). Not part of the per-author hash chain or DAG (presence-like).
The wire carries **no** behavior discriminator: every task is identical to the
binary, and the two delegation UX flows below distinguish themselves in-band.

Two skills ride this primitive. `/swarm:task` is the **report-back** flow — the
worker does the work and returns its result on `done`, and the initiator
confirms (or `change`s for a revision); it sends one or more independent tasks
(each its own `task_id`, worker, and completion criteria) and surfaces each
result as it returns, with no group-level outcome. `/swarm:handover` is the
**walk-away** flow (see below).

**Keepalive vs. liveness.** While the ball-owner is silent, its daemon emits a
`progress` keepalive so a genuinely-working owner is not falsely timed out. But
the keepalive is bounded by **skill** liveness, not process liveness: it only
fires while a real leg has been driven within `TASK_KEEPALIVE_MAX_SECS` (a leg
the daemon's own keepalive never counts as). Past that, the keepalive stops and
the peer's debounce reaps the task — so a crashed or abandoned skill cannot hold
the peer forever. A skill doing very long silent work refreshes the window by
sending its own `progress` beat.

Code: `MessageKind::Task`, `lifecycle::handle_task`, `broadcast_task`,
`daemon::task` (`TaskRecord::should_keepalive`).

### handover

*Layer: skill behavior on top of **task**.*

A UX behavior on the task primitive, driven entirely by the `/swarm:handover`
skill: delegate a task/plan and walk away. The receiver runs the work **itself**
after the handoff and the initiator **auto-confirms** — no result flows back
(the difference from `/swarm:task`, which returns a result). It uses the same
task phases (`offer → accept → context → done → confirm`), ending at the close
handshake. Because the wire has no `kind`, the "walk-away vs report-back" intent
travels **in-band** — a marker in the `offer` body (and the skill's todo text) —
not as a wire field. Adds no wire type of its own.

### part

*Layer: protocol — a header on **message**.*

One slice of a body too large for a single gossip message. When a `msg` or a
task leg's body exceeds `MAX_MESSAGE_SIZE`, the sender splits it into several
ordinary signed messages, each carrying a `part` header — a `group` (a UUID
shared by the body's parts), an `idx`, and the `total` count. Each part is a real
message (own id/seq/signature) retained in the **message log**, so a missing part
heals through anti-entropy like any message. The receiver reassembles the parts
of a `group` (keyed also by author key, so a crafted cross-author part can't
inject a slice) into the one logical message it surfaces; the raw parts never
surface. Capped at `MAX_MESSAGE_PARTS` per body — a larger body is refused on
send. The split is invisible to agents: a body sends and arrives whole.

### shared state

*Layer: state · two **channels** per swarm (`state`, `meta`), each a document
derived from its own **state log**.*

A JSON document the whole swarm shares, separate from the chat message log. It
is never sent whole on the wire: every member **derives** it by folding the
**state log** (the `(timestamp, id)`-ordered replay of every **change**) from
`{}`. Same event set ⇒ byte-identical document on every member (see the *Shared
state converges deterministically* invariant).

Each swarm carries **two channels**, `state` and `meta` — byte-for-byte the
same machinery (same reducer, log, anti-entropy, RFC 7386 merge rules),
differing only by **convention**: `state` is the task working area;
`meta` holds swarm metadata, by convention `/peers/<nick> = { model, harness,
host, status }` that each agent self-reports (`host` is the machine's
self-reported hostname; `status` is its availability — `idle`/`available`/`busy`,
where `busy` means "not accepting work" and the delegation pickers skip it). The
binary does **not** differentiate them and
never writes a channel itself — the **only** way to change either is a JSON
merge (`ahsw state merge` / `ahsw meta merge`). Read with `ahsw state get` /
`ahsw meta get`. A change surfaces as the `state` / `meta` event, carrying both
the merge and the newly-derived document.

Code: `daemon::state_doc` (the reducer `JsonDoc` + `derive_document`),
`protocol::Channel`, `OutputEvent::StateChanged`.

### state log

*Layer: state · `MessageKind::State` / `MessageKind::Meta`, one un-pruned,
unbounded store per **channel**.*

The signed channel events a swarm folds into a **shared state** document — one
log per channel (`State` for `state`, `Meta` for `meta`), distinct from the chat
**message log** in three ways: each is **un-pruned and unbounded** (the fold
needs the complete set, so nothing ages out), dedup-keyed by id, and reconciled
by its **own** anti-entropy digest (windowed like the chat digest, but
advertised open at both ends so a late joiner backfills the *whole* log, not
just a recent tail). Bounding total growth (compaction/snapshots) is deferred.

Code: `daemon::state_log::StateLog`, `gossip::antientropy::{broadcast,handle}_state_digest`.

### change (state merge)

*Layer: state · an RFC 7386 JSON Merge Patch document in a `State` event body.*

One modification to the **shared state**: an RFC 7386 JSON Merge Patch — any
JSON value applied to the document. An object deep-merges (each key set; a
`null` value deletes that key; nested objects merge recursively; arrays are
replaced wholesale), and a non-object value (scalar/array/`null`) replaces the
target, including the document root. There is no validation and no rejection: any
JSON value is a valid merge. Merge is not commutative, but every member folds the
same log in the same `(timestamp, id)` order, so all converge; because each
writer touches only its own keys, concurrent writers to different keys never
clobber.

Code: `daemon::state_doc::{merge_body, apply_merge_body, merge_into}`.

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
participant lifecycle. (`ahsw leave` is a CLI verb on top of this
vocabulary, not a new event: it stops a local daemon, whose shutdown emits
the one `left`.)

### author

The `Nickname` that wrote a message. It is the same value-type as a
participant id; the distinct word marks "sender of *this* message", not a
separate concept.

### Shared state converges deterministically

Every member derives the **shared state** by folding the **state log** in one
total order — `(timestamp, id)` — that every member computes identically, with a
failed/out-of-subset **change** as a deterministic no-op. So the document is a
pure function of the *set* of changes: the same set always yields the
byte-identical document, regardless of arrival order. Convergence is
unconditional.

*Causal faithfulness* is the weaker, conditional property: that a change lands
*after* the change it depends on. The timestamp is one-second resolution, so two
changes to the **same key** authored in the same second can sort by the `id`
tiebreak in either order — convergent, but the one that "wins" (folds last) may
not be the one a reader intended. Phase 1 resolves this by **timing, not a
clock**: changes are turn-based (seconds apart) and a member changes the state on
its turn, so dependent changes are naturally separated; multi-key updates that
must land together go in **one** merge object. Sub-second concurrent multi-writer
causality is out of scope (a future causal DAG via per-author `seq`/`parents`).

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
