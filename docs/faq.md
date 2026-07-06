# FAQ

> 🚧 **Under construction.** This document is a work in progress and may be
> incomplete or out of date.

## If two people create a mesh with the same name, is it the same mesh?

No. A mesh's identity is a **random 32-byte seed** minted at `create` time,
not its name. Two people running `agent-mesh create --name demo` get different seeds,
hence different `💬…` hashes and different gossip topics — two independent
meshes that never discover or message each other.

The name (and the rest of the config) is mixed into the topic derivation so
that a **forged** hash — someone takes a real `💬…` id and flips the name or a
config bit — derives a different topic and finds no peers. So the name is
*tamper-evident*, not the source of identity. See
[`mesh-hash.md`](mesh-hash.md).

The one deliberate exception is a **directory**, whose mesh seed is derived
*deterministically* from its name (`derive_secret(DIRECTORY_BASE_SEED, name)`), so
everyone naming the same directory shares the same seed and rendezvous. Its
*topic* still mixes in the lookups in use, so an advertiser and a discoverer
meet only when they use the **same** lookups (`agent-mesh discover --mdns` finds an
`--mdns`-only advertiser; the all-on default on both sides meets too). That is
how `--advertise` / `agent-mesh discover` rendezvous by name — see
[`discovery.md`](discovery.md). If your goal is "find a mesh by name," the
mechanism is advertise-into-a-directory + discover, not a name collision on the
hash.

## Can `agent-mesh discover` list two meshes with the same name?

Yes. A directory keys its listings by **mesh id**, not by name, so two meshes
both named `demo` (different seeds ⇒ different `💬…` ids) appear as two
separate entries that both display the name `demo`.

This is purely cosmetic in the name column — the `💬…` id is the real
identifier everywhere: the interactive picker shows the full id next to each
name (and joins the highlighted row by its id), and `--output json` carries
both `name` and `mesh` (the id) on every `mesh_found` line. Each is joined
independently by id.
