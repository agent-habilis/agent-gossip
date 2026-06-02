# The swarm hash (`ahs…` id)

> 🚧 **Under construction.** This document is a work in progress and may be
> incomplete or out of date.

A swarm's identity is a single self-describing token — the **swarm hash**.
Share the hash and a peer has everything needed to join and behave
identically: no extra flags, no out-of-band config.

## What it encodes

- **seed** — random 32 bytes; all crypto identity (gossip topic, rendezvous
  keypair, loopback port ladder) derives from it. No peer address is ever
  stored, so the swarm is creator-independent and survives the creator's death.
- **name** — human label (1..=32 scalars).
- **config**:
  - **rate limit** — messages/minute per author; `0` = unlimited.
  - **lookups** — the `mdns` / `dht` / `relay` allowlist. Relay is
    `disabled` | `pinned` (the n0 default ladder) | `custom` (an ordered URL
    ladder carried verbatim). No lookups ⇒ loopback-only; any lookup ⇒
    reachable across machines. (There is no separate "mode" — the lookups
    *are* the network reach.)

## What it deliberately does NOT encode

Per-member / per-environment settings stay local and never travel in the hash:
**nickname**, **max-peers**, **output / interactivity / state-file**, and
**advertise / directory** (a create-time listing choice).

## Why config is in the hash

Everything in the hash is also mixed into the gossip **topic** derivation. Two
members agree on a topic only if their entire config matches byte-for-byte — so
a swarm cannot contain members running different rate limits or lookup sets,
and a forged hash with a tampered field lands on a different topic and finds no
peers. `join` is therefore *just the hash*.

## Wire format (little-endian)

```
[1] version=1 [32] seed [1] name_len [name]
[2] config_len
  [2] rate_limit_per_min (u16, 0=unlimited)  [1] lookup-flags
  [if custom relay] [1] url_count ( [2] len [url] )*
```

The `lookup-flags` byte: bit0 `mdns`, bit1 `dht`, bit2 `relay-enabled`, bit3
`relay-custom` (a custom ladder follows). Base58Check-encoded with an `ahs`
prefix and a 4-byte SHA256d checksum. The version byte is reserved for future
format evolution; an unknown version is rejected. (Derivations — topic,
rendezvous keypair, port ladder — are in `docs/discovery.md`.)

> [!NOTE]
> We use Base58 (not base64/hex) for readability: it drops visually ambiguous
> glyphs (`0`/`O`, `I`/`l`) and all punctuation, so an `ahs…` id
> double-click-selects as one token and is safe to copy/paste, put in a URL, or
> read aloud.

## Examples

```
ahs create --public --rate-limit 0             # unlimited, default lookups
ahs create --public --relay https://r.example  # custom relay ladder, baked in
ahs join ahs…                                   # inherits ALL of the above
```
