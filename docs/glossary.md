# Concept Glossary

> 🚧 **Under construction.** This document is a work in progress and may be
> incomplete or out of date.

One concept, one word. The codebase is organized in layers, and each layer
owns a term it never lends to another. Keeping these distinct is what stops a
transport detail from leaking into membership prose, or a display label from
being mistaken for an identity. When reading or changing code, hold the
following meanings.

For the mechanisms behind these terms, see the companion docs:
[swarm-hash.md](./swarm-hash.md) (the `ahs…` token),
[discovery.md](./discovery.md) (rendezvous, beacon, lookups, directories),
[gossip.md](./gossip.md) (message fan-out),
[topologies.md](./topologies.md) (network shapes), and
[history-integrity.md](./history-integrity.md) (signing and fork detection).

## Terms

### endpoint / link

*Layer: transport · keyed by node id (hex).*

An iroh `EndpointId` and the gossip neighbor link to it. This is pure
plumbing — it is never surfaced to operators or agents.

State: `linked_endpoints`.

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

The `ahs…` id: a self-describing token carrying the `seed`, the name, and the
swarm's **config** (rate limit plus lookups). The config is mixed into the
gossip topic, so every member necessarily shares it, and `join` needs nothing
beyond the hash itself.

Code: `protocol::swarm` (`Swarm` / `SwarmConfig`). Byte layout:
[swarm-hash.md](./swarm-hash.md).

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
message is signed with this key and verified on receipt, and both the rate
limit and fork detection key on it. It is distinct from the shared
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
name)`) that swarms **advertise** their `ahs…` id into and that **discover**
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

Browse a directory's live swarms (`ah-s discover`) and join one — the consumer
side of **advertise**.

### exchange

*Layer: messaging · keyed by `exchange_id` (correlation) + the two parties' nicknames.*

A typed, phased, directed conversation (`MessageKind::Exchange`, phases
`offer`/`accept`/`decline`/`context`/`progress`/`done`/`confirm`/`change`/`cancel`)
correlated by an `exchange_id`, with a `kind` discriminator (`handover` |
`task`). The generic **mechanism**: a directed, multi-leg conversation
whose *coarse* lifecycle (phase advance, the per-exchange idle-debounce timeout,
the ball-owner keepalive, the 100-content-message cap) is owned by the daemon
state machine (`daemon::exchange`), while the *content* is owned by the skill. Like
a directed `Msg --reply`, a leg is delivered to all members for relay but
**surfaced and logged only by its addressee and the sender's own echo** — a
third party never sees it. Content legs are rate-limited with `Msg`; the
`progress` phase is liveness plumbing (rate-limit-exempt, never logged). Not
part of the per-author hash chain or DAG (presence-like).

Code: `MessageKind::Exchange`, `lifecycle::handle_exchange`, `broadcast_exchange`,
`daemon::exchange`.

### handover

*Layer: behavior on top of **exchange**.*

The behavior that delegates a task/plan to another agent — `ExchangeKind::Handover`
on the exchange mechanism, driven entirely by the skill (`/swarm:handover`). It
runs `offer → accept → context → done → confirm`, ending at the close
handshake. "A handover is a behavior that uses the exchange mechanism"; it adds no
wire type of its own.

### task

*Layer: behavior on top of **exchange**.*

The behavior that runs work and **returns the result** — `ExchangeKind::Task`
on the exchange mechanism, driven by the skill (`/swarm:task`). It runs
`offer → accept → [context] → done → confirm`, where the worker reports its
result on `done` and the initiator confirms (or `change`s for a revision) —
the difference from **handover**, which closes without a result. `/swarm:task`
sends one or more independent tasks (each its own `exchange_id`, worker,
and completion criteria) and surfaces each result as it returns; there is no
group-level outcome. Like handover, it adds no wire type of its own.

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
participant lifecycle.

### author

The `Nickname` that wrote a message. It is the same value-type as a
participant id; the distinct word marks "sender of *this* message", not a
separate concept.

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
