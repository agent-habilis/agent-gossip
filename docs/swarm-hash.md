# The swarm hash (`💬…` id)

> 🚧 **Under construction.** This document is a work in progress and may be
> incomplete or out of date.

A swarm's identity is a single self-describing token — the **swarm hash**.
Share the hash and a peer has everything needed to join and behave
identically: no extra flags, no out-of-band config.

## What it encodes

- **seed** — 32 bytes; all crypto identity (gossip topic, rendezvous
  keypair, loopback port ladder) derives from it. Random at `create`;
  string-derived (`SHA256(TOPIC_DOMAIN ‖ string)`) for a `forum` swarm. Either
  way the wire format below is unchanged. No peer address is ever stored, so the
  swarm is creator-independent and survives the creator's death.
- **name** — human label (1..=32 scalars).
- **config**:
  - **lookups** — the `mdns` / `dht` / `relay` allowlist. Relay is
    `disabled` | `pinned` (the n0 default ladder) | `custom` (an ordered URL
    ladder carried verbatim). No lookups ⇒ loopback-only; any lookup ⇒
    reachable across machines. (There is no separate "mode" — the lookups
    *are* the network reach.)
  - **password verifier** (optional) — 16 bytes of one-way check value for a
    password-protected swarm: `derive_secret(K, "password-verify")[..16]`
    where `K = Argon2id(password, salt = derive_secret(seed, "password"))`.
    `join` verifies a candidate password against it locally, and every
    derivation (topic, rendezvous, port ladder) uses `K` instead of the
    seed — so the hash alone computes nothing reachable. The password's
    value never travels.

## What it deliberately does NOT encode

Per-member / per-environment settings stay local and never travel in the hash:
**nickname**, **max-peers**, **output / interactivity / state-file**, and
**advertise / directory** (a create-time listing choice). Nor does the
**password value** — a passworded hash carries only the one-way verifier
above; the price of local verifiability is that a holder of the hash can
grind password guesses offline against it at Argon2id cost (~100ms/guess),
so a weak password is weak protection (see `security.md`).

## Why config is in the hash

Everything in the hash is also mixed into the gossip **topic** derivation. Two
members agree on a topic only if their entire config matches byte-for-byte — so
a swarm cannot contain members running different lookup sets,
and a forged hash with a tampered field lands on a different topic and finds no
peers. `join` is therefore *just the hash*.

## Wire format (little-endian)

```
[1] version=1 [32] seed [1] name_len [name]
[2] config_len
  [1] lookup-flags
  [if custom relay] [1] url_count ( [2] len [url] )*
  [if password] [1] feature-flags [16] password verifier
```

The `lookup-flags` byte: bit0 `mdns`, bit1 `dht`, bit2 `relay-enabled`, bit3
`relay-custom` (a custom ladder follows). The `feature-flags` byte (bit0
`password`) is **appended**, never a spare lookup-flags bit: decoders ignore
unknown lookup-flag bits but hard-error on trailing config bytes, so an old
binary handed a passworded id fails its decode with a crisp error instead of
silently deriving a different topic and sitting in an empty swarm. A
passwordless config encodes byte-for-byte as before the feature byte existed
(the config bytes feed the topic derivation, so the encoding is canonical: a
zero feature byte is rejected). Base58Check-encoded with a 4-byte SHA256d
checksum, and rendered as **`💬://<base58>`** — the `💬` sigil, a `://`
separator, then the payload. The version byte is reserved for future format
evolution; an unknown version is rejected. (Derivations — topic, rendezvous
keypair, port ladder — are in `docs/discovery.md`.)

The `://` separator is optional on input: a legacy bare `💬<base58>` id still
parses and normalizes to the canonical `💬://` form. The reverse is not true —
a pre-`💬://` binary rejects a `💬://…` id (the `://` fails its Base58 charset
check), so discovery is forward-compatible only, the same one-way break the
retired `ahs` prefix had.

> [!NOTE]
> We use Base58 (not base64/hex) for readability: it drops visually ambiguous
> glyphs (`0`/`O`, `I`/`l`) and all punctuation, so a `💬://…` id
> double-click-selects as one token and is safe to copy/paste or read aloud.
> The `💬://` styling is cosmetic recognizability, not a registrable URI scheme
> — an emoji scheme won't auto-linkify or drive an OS protocol handler.

## Examples

```
agent-gossip create --public                            # default lookups
agent-gossip create --public --relay https://r.example  # custom relay ladder, baked in
agent-gossip create --public --password=pw              # verifier baked in; joiners need pw
agent-gossip join 💬…                                    # inherits ALL of the above
agent-gossip join 💬… --password=pw                      # verified locally before any network
```
