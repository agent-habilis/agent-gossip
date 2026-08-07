# Nicknames are not identities

Every party check in the A2A layer compares nicknames. A nickname is a
self-asserted label with no binding to a key, so those checks are decorative
against a mesh member willing to type a different name into a frame.

This is not a bug in one function. It is a gap between two layers that each
behave correctly on their own terms, and closing it needs a change in the
engine. This note records the shape of the problem so the next person does not
have to re-derive it, and so the app-side patches that *have* landed are not
mistaken for a fix.

## What is actually true today

The engine authenticates the **key**. `gossip/recv.rs` verifies every inbound
frame's signature against the pubkey the frame carries, and says so plainly:

> Identity is the signing key, not the nickname (p2panda-style): the signature
> above authenticates the *key*; the `author` nickname is a non-unique display
> label and is deliberately **not** pinned/claimed, so a nickname is never
> "burned" by a restart on a long-lived mesh. Identities are distinguished by
> their key fingerprint, not the name.

That is a deliberate decision with a real motivation. Nicknames are short,
memorable and reusable; pinning them means a peer that restarts with a fresh
key can never reclaim its own name, and on a long-lived mesh the useful names
are consumed by ghosts.

The app then builds authorization on top of the label:

- `task.rs` — `TaskRecord.peer` is a `Nickname`, and every party check compares
  against it (`node.rs`'s `handle_task_leg` and `ingest_remote_message`,
  `gossip_rpc.rs`'s `classify`, `task.rs`'s `apply` and `adopt_initiator`).
- `card.rs` — the seal key and the endpoint hint are looked up at
  `/peers/<nick>/card`, so a nickname decides both who can read a directed
  frame and where its bytes are dialed.
- `node.rs` — `handle_msg` gates on `to == self_author`.

Nothing pins `message.author` to `message.pubkey`. The two layers are each
self-consistent; the seam between them is where the authority is lost.

## Why the card cannot fix it on its own

The card *does* publish the pubkey — `supportedInterfaces[0].url` is the
author's hex key — so the binding data exists. The obvious app-side fix is:
when a frame claims nickname `alice`, check the frame's pubkey against the key
in `/peers/alice/card`.

That fails, because the card is guarded by the same label it is supposed to
authenticate. `a2a/mod.rs` installs

```rust
SelfWriteGate { map: "peers", field: "card" }
```

and the engine's `fofoca-doc` implements it as: reject a change that alters any
entry whose nickname differs from `frame.author`. So the gate stops *Mallory
writing to alice's entry under her own name*. It does not stop *Mallory signing
a frame that claims `author: "alice"`* — signature verification passes, because
the author field is signed, and `forges_foreign_entry` sees a touched nick equal
to the frame's author and waves it through.

The consequence is worth stating concretely, because it is the sharpest edge
here: an attacker rewrites `peers/alice/card` with their own x25519 key and
their own `EndpointId`. From then on every peer's directed traffic to Alice is
sealed to the attacker and unicast to the attacker's endpoint. The attacker
reads everything; Alice receives nothing. Every nickname-keyed party check
downstream inherits this.

Checking a card against itself is circular. The card is only trustworthy once
the binding exists, and the binding is what we are trying to establish.

## Why a first-writer pin, app-side, is the wrong shape

The tempting workaround is trust-on-first-use in the app: remember the first
pubkey seen for each nickname and drop later frames that claim it with a
different key.

It does close the impersonation surface for the lifetime of a process, and it
needs no engine change. It is still the wrong thing to land here:

- It re-implements, in one consumer, the pinning the engine deliberately
  refused — without the engine's reasons being addressed or revisited.
- It breaks the case the engine's decision exists to protect. A peer that
  leaves and rejoins under the same nickname with a fresh key is
  indistinguishable, to a TOFU pin, from an attacker. The rejection is silent
  and total: the peer is on the mesh, visible in the roster, and every frame it
  sends is dropped by everyone who saw the earlier key.
- It converges nowhere. Each node pins whatever it happened to see first, so
  two nodes that joined at different times can disagree about who `alice` is,
  with no mechanism to reconcile. The current design's saving grace is that
  every honest member runs the *same* gate and therefore agrees; a per-node pin
  gives that up.

A pin that is per-node, unversioned and unreconcilable is not a weaker version
of the right fix. It is a different and worse property.

## What the real fix looks like

The gate has to authorize on the **writing key**, not the written name. In
`fofoca-doc`, `SelfWriteGate` would carry the identity it is guarding rather
than inferring it from the frame's self-declared author: the first change that
creates `<map>/<nick>` binds that entry to the pubkey that authored it, and
every later change touching it must come from the same key.

That keeps the engine's "the engine needs the gate's shape and rule, never its
meaning" property — it is still comparing before and after, still knows nothing
about cards or A2A — while making the comparison one an attacker cannot satisfy
by renaming themselves.

It also needs an answer for the case the current design protects, and this is
the part that deserves design attention rather than a quick patch: what happens
when a peer legitimately returns under the same nickname with a new key.
Plausible directions, none obviously correct:

- Let the entry expire with its peer, so the name frees on eviction and the
  binding is per-session rather than forever.
- Make the rebind explicit and visible — a name change that every member
  observes, rather than a silent substitution.
- Keep the nickname unpinned for display, and give the security-relevant paths
  (seal key, endpoint, task parties) a separate key-addressed identifier, so
  the label stays cheap and reusable while authorization stops depending on it.

The third is the closest to the engine's stated model, and probably where this
should land: it does not contradict the decision quoted at the top, it just
stops the app from leaning on a value the engine never promised.

## Scope

`fofoca` is a separate repository with two other consumers, `agent-share` and
`mallorca` (through `fofoca-ffi`'s C ABI). A change to the doc gate is a change
to all three. That is why it is written up rather than done in passing here.

## What has landed, and what it does not cover

Two app-side patches reduce the blast radius without closing the gap:

- `handle_msg` no longer admits a frame on `author == self_author`. That clause
  was documented as the sender's echo path, but the echo is rendered from a
  plaintext twin on the send path and the engine drops our own frames by pubkey
  before the receive hooks — so the clause was unreachable for honest traffic
  and satisfiable only by a forgery. It let a relayed msg between two other
  peers surface as one we had sent.
  (`adversarial.rs`'s `msg_forged_under_our_own_nickname_is_not_surfaced_by_a_relay`)
- The task plane now checks the leg's author against the record's counterparty
  in `apply` and `handle_task_leg`, and the RPC reads are party-checked.

Neither touches the card. An attacker who overwrites `peers/<victim>/card`
still redirects that victim's sealed traffic, and the
`gap_nickname_impersonation_is_accepted` tripwire in `adversarial.rs` still
describes a live gap. The task-plane checks above are worth having — they close
the paths that need no impersonation at all — but a determined attacker who
takes over a nickname's card satisfies them too, because they are also
nickname comparisons.

Treat `gap_nickname_impersonation_is_accepted` going red as the signal that
this note is obsolete. Until then it is not.
