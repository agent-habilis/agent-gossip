# Message-history integrity

> 🚧 **Under construction.** This document is a work in progress and may be
> incomplete or out of date.

How `agent-habilis-swarm` makes its message history **authentic and
tamper-evident** without a server, a blockchain, or consensus.

This is the mechanism companion to [`security.md`](./security.md) (the
threat-model summary), [`swarm-hash.md`](./swarm-hash.md) (the `🐝…` token),
and [`gossip.md`](./gossip.md) (why every member receives every message).

> **Status:** implemented. The message envelope carries the fields below; an
> honest node rejects any unsigned or invalid message.

---

## Goals

- **Authenticity** — a message is provably from the holder of an identity key.
- **Integrity** — no byte of a message can be altered undetected.
- **Tamper-evident ordering** — an author cannot rewrite, reorder, or
  truncate their own log undetected; cross-author causal order is verifiable.
- **Mergeable** — divergent histories (partition, sleep-wake, late join)
  re-converge by set union; no message is ever silently dropped.
- **From-join-onward** — these hold from a node's own join; ancient,
  pre-join history is *not* retroactively authenticated (no root-of-trust).

## Non-goals

Confidentiality (members still see all messages), access control (the `🐝…`
id stays a bearer capability), censorship *prevention* (only detection),
true wall-clock timestamps, and global total order.

---

## Data model & prior art

The structure is the standard **secure append-only log**, assembled from:

- a per-author **Secure Scuttlebutt (SSB) feed** — a signed hash chain
  (`seq` + `prev`), single-writer and append-only;
- a cross-author **Merkle-DAG** (`parents`) — the shape of a Git commit
  graph / Matrix event DAG / Hashgraph — for verifiable *partial* (causal)
  order;
- a **grow-only set (CRDT)** reconciled by gossip **anti-entropy**, so
  divergent histories merge with no coordinator and no canonical chain.

The reference shape is **p2panda-core**'s `Header` (`seq_num`/`backlink` +
DAG-via-extension `namakemono`), implemented on in-tree primitives (iroh's
Ed25519 `SecretKey`, `sha2`) rather than taken as a dependency. The deliberate
*non*-choice is a global-consensus **blockchain**: a chat needs authentic,
causally-ordered, mergeable history, not one linear chain bought with
proof-of-work and consensus latency — and a single chain would orphan the
"losing" branch's real messages.

---

## Identity

Each participant holds a per-swarm **Ed25519 keypair**, separate from the
transport `EndpointId` and the shared seed-derived rendezvous key (the first
*per-author* credential in the system).

- **Generated** on first `create`/`join`.
- **In-process / ephemeral** (current): held in memory for the process
  lifetime, not written to disk. A process *restart* therefore mints a new
  key — a new identity / fingerprint — though it may reuse the same display
  nickname freely (names are never claimed). **Persistence** (a stable
  on-disk key keyed by `(swarm_prefix, nickname)`, outside the wiped `/tmp`)
  is a follow-up that makes a reconnect re-present the *same* identity
  instead of a new one.
- **Identity = the public key** (its short **fingerprint** — a prefix/hash
  of the key — is the human-facing id). The nickname is a **non-unique
  display label**: freely chosen, never claimed, never pinned. Two
  identities may show the same nickname; they are told apart by fingerprint.
  This is the p2panda model — trust attaches to the key, not the name.

Because nicknames are never claimed, **a nickname is never "burned"** by a
restart: a restart mints a new key (a new identity / fingerprint) but the
display name stays freely reusable. (Persisting the key — a follow-up — would
additionally make a restart re-present the *same* identity rather than a new
one; it is not needed to avoid burning names.)

Key generation is invisible plumbing; the "pick a nickname" UX is unchanged.

---

## Wire envelope

The JSON `Message` carries these fields:

| Field | Type | On which kinds | Phase | Meaning |
|---|---|---|---|---|
| `pubkey` | Ed25519 public key (hex) | all | 1 | author identity key |
| `sig` | Ed25519 signature (hex) | all | 1 | over the canonical signed bytes |
| `seq` | u64 | `Msg` | 2 | per-author monotonic counter |
| `prev` | hash (hex) or null | `Msg` | 2 | content hash of author's previous `Msg` (`null` at `seq 0`) |
| `parents` | array of hash | `Msg` | 3 | DAG tips seen when authoring (bounded) |

Only **`Msg`** carries the chain fields (`seq`/`prev`) — presence and plumbing
(`Alive`, `Digest`, `Ping`, `Pong`, `PeerInfo`) are **signed** (Phase 1) but
**not** chained: presence is re-broadcast with fresh ids, which doesn't fit a
linear per-author chain, and the valuable target is the author's chat stream.

The **content hash is computed locally, not transmitted** — any receiver
recomputes `SHA-256(canonical_bytes)`, so it is never a tamperable wire field;
`prev` references the *previous* message's locally-computed hash. The
random-UUID `id` is **kept** as the dedup/cursor key, so the IPC contract
(`poll --after {UUID}`, the MCP implicit cursor) is untouched.

### Canonical bytes

Signing and the content hash use a **deterministic, length-prefixed
concatenation** of the signed fields — `version, id, kind, swarm, author, ts,
body, pubkey, seq, prev, ext` (everything except `sig`) — not `serde_json`
(which is not canonical), domain-separated like `crypto::derive_secret`.
`content_hash = SHA-256(canonical_bytes)`; `sig =
Ed25519_sign(secret, canonical_bytes)`. `None` `seq`/`prev` encode as
zero-length fields, distinct from `Some(0)`.

---

## Data structures

The existing log stays:

```rust
struct MessageLog { capacity: usize, messages: VecDeque<Message> }  // cap 200, by-UUID
```

**Phase 2** adds two fields to `EventLoopState` (in `src/daemon/state.rs`):

```rust
// Our own send-side chain cursor:
self_seq:  u64,            // next Msg seq to emit
self_prev: Option<String>, // content hash of our last Msg

// Fork detection over other authors' Msg streams:
author_seqs: HashMap<String /*pubkey hex*/, HashMap<u64 /*seq*/, String /*hash*/>>,
forked:      HashSet<String /*pubkey hex*/>, // flagged once per key
```

`note_msg_seq(pubkey, seq, hash)` records each `Msg` and returns `true` the
first time a *different* hash collides at an already-seen `(pubkey, seq)` —
the equivocation proof. (Phase 3 adds the DAG index — `by_hash`, current
`heads`, `parents` — over a content-hash graph; not built yet.)

There is no nickname→key pin map: identity is the key, and nicknames are
non-unique display labels that are never claimed — see [Identity](#identity).

### Bounding the identity maps

`author_seqs` and `forked` are keyed by **identity**, not by message, so they
are *not* bounded by the log — and this is an **open** swarm, where an attacker
can mint unlimited fresh keypairs and send one signed message each (a sybil /
memory-DoS flood). Signatures do not help: each fake key is "valid," just
worthless. **Bound implemented — tied to the log window:**

1. **GC to the log horizon.** Every index entry is pruned when the message
   that created it is evicted from the 200-message `VecDeque`: `forget_hash`
   drops `by_hash`/`dag_heads`, and `forget_msg_seq` drops the `(pubkey, seq)`
   from `author_seqs` — and when an identity's last logged message ages out,
   it is removed from `author_seqs` **and** `forked`. So a sybil that fires
   once and vanishes is forgotten; total retained identities ≤ `|log
   authors|` ≤ 200. No separate cap is needed — the log *is* the bound.

---

## Verify pipeline (on receive)

In `handle_gossip_received`, **before** accept/relay/log/surface (right after
`Message::parse`, before `mark_seen`):

1. **Signature** — recompute canonical bytes, verify `sig` against `pubkey`.
   Fail → drop, do not relay, warn-log. The signature authenticates the
   **key**; the `author` nickname is cosmetic and is *not* validated against
   it (identities are keyed by `pubkey`, distinguished by fingerprint).
2. **Fork detection** (`Msg` only) — record `(pubkey, seq) → content_hash`.
   A *different* hash at an already-seen `(pubkey, seq)` is proof of
   equivocation → emit a `fork` event once per key (`forked` set); **keep
   both** messages, never auto-pick. This is **order-independent** (gossip is
   unordered), so it does **not** strictly validate `prev`-chaining — `prev`
   is carried/signed for tamper-evidence and Phase 3, but enforcing it on
   receive would false-positive on reordered/late delivery.
3. **DAG** (Phase 3) — for each `parents` hash absent locally: flag a missing
   ancestor and let anti-entropy pull it (parents make backfill precise).
4. **Timestamp** (Phase 3) — reject/flag `ts < max(parents.ts)` (bounds
   backdating).

Status: all four steps are implemented — **1 (signature)**, **2 (fork
detection)**, **3 (DAG tip-set update)**, **4 (backdating flag)**. Step 4
warns rather than drops (clock skew shouldn't censor); a missing parent in
step 3 is left for the id-keyed anti-entropy to converge, not actively
pulled.

---

## Display linearization (presentation-only, deferred)

A *full-history render* can be a **deterministic linearization** of the DAG:
a topological sort (Kahn) over the in-memory set, concurrent siblings ordered
by `(timestamp, hash)` — a pure function of the set, so every node renders an
identical order.

This is **not applied to the live stream**, and that is deliberate:
`poll --after {id}` / the `--output json` stream / the MCP cursor deliver
messages **incrementally**, and you cannot reorder a message already delivered
past the cursor (a late-arriving causal predecessor would need to insert
*behind* the cursor). So the live stream stays **arrival-ordered** (≈
timestamp); topological linearization is a presentation-layer feature for a
full re-render, layered on later with no protocol change. The DAG's value here
is the **verifiable causal structure** (`parents`) + backdating bound, not
re-sorting the stream.

---

## Conflicting histories

No single canonical chain ⇒ divergent histories **union**, never elect a
winner (a chat must not drop a message):

- **Benign divergence** (partition / sleep-wake / late join): histories are
  complementary, not contradictory. On heal, anti-entropy exchanges the
  missing messages; each side adds the other's as new DAG nodes (a diamond:
  common ancestor → two branches → a later message naming both heads as
  `parents`). Views re-converge.
- **Equivocation** (one key, two messages at the same `seq`): a **fork**. The
  signed pair is self-contained proof, so the instant both branches reach any
  honest node, a `fork` event fires naming the key. Detected, **never
  auto-resolved** — there is no trustless winner, and resolving one would
  reward the attacker.

Anti-entropy intake signature-verifies every backfilled message before it
enters the DAG, so a peer cannot poison local history during backfill; the
merge is a union of **verified messages only**.

---

## Limits (the honest residue)

- **Omission / censorship** is detectable (DAG/chain gaps) and repaired by any
  one honest peer, so it only succeeds under a **total eclipse** — but cannot
  be prevented.
- **Timestamps** are non-repudiable, not *true*; only relative order (`seq`,
  `parents`) is enforced.
- **Pre-join history** is not retroactively authenticated: every backfilled
  message's signature is checked, but a malicious backfiller can mint a fresh
  key and fabricate an entire identity + history you never saw live — you
  cannot tell it from a real participant who left before you joined. Fixing
  this would need a creator-rooted roster — omitted.
- During an active partition/eclipse a node cannot know what it is missing;
  the guarantee is **convergence on heal + detection of malice**, not
  real-time global truth.
- **Membership is still nickname-keyed.** The participant roster, presence
  (`joined`/`left`), and heartbeat (`peer_timeout`/`peer_return`) key on the
  nickname, so two identities sharing a display name **collapse** in the
  roster. Message *authenticity and fork detection* are
  keyed on the **pubkey** and unaffected (and a same-named peer's messages are
  delivered — self-echo is keyed on our key, not name). Re-keying the
  membership/lifecycle layer on the pubkey is a follow-up.

---

## JSON events

The `message` and presence events carry the author's full Ed25519 public key
(hex) as `pubkey` — the cryptographic identity behind the display `author`.
Agents key trust/disambiguation on `pubkey`, not the (non-unique) nickname.
The human/TUI rendering is unchanged; only the `--output json` stream gains
the field. **(Implemented.)**

```json
{"event":"message","id":"uuid","type":"msg","swarm":"🐝://...","author":"nick","pubkey":"<64-hex>","ts":1234567890,"body":"hello","reply":null,"self":false}
```

A new `fork` event (Phase 2) is emitted once per offending key when
equivocation is detected, on the `--output json` stream:

```json
{"event":"fork","nickname":"word-word","pubkey":"<hex>","seq":42}
```

---

## Rollout phases

1. **Signatures (key = identity)** — keypair, `pubkey` + `sig`,
   verify-on-receive; nicknames are non-unique
   display labels (no pinning, never burned). Kills key impersonation, body
   tampering, on-path tampering. **(Implemented.)**
2. **Per-author log** — `seq` + `prev` on `Msg`, locally-computed content
   hash, equivocation (`fork`) detection + `fork` event. **(Implemented.)**
3. **Cross-author DAG** — signed `parents` (causal links), local tip-set
   tracking, the `ts ≥ max(parents.ts)` backdating flag. **(Implemented.)**
   *Not* done: parent-*driven* backfill (the id-keyed anti-entropy already
   converges the set, so missing parents fill in regardless) and full-render
   topological linearization (incompatible with the incremental `poll
   --after` cursor — see [Display linearization](#display-linearization)).
