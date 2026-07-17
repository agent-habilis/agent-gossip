# Documentation

In-depth docs for agent-gossip.

- [Concept glossary](./glossary.md) — the one-word-per-concept vocabulary, the
  layering, and the invariants that follow from it.
- [The mesh hash (`💬…` id)](./mesh-hash.md) — byte layout of the
  self-describing `💬…` id (seed + name + config).
- [How peers find each other](./discovery.md) — rendezvous, the beacon role,
  the mDNS/DHT/relay lookups, and directories.
- [How the gossip protocol works](./gossip.md) — HyParView membership +
  Plumtree-style message fan-out over iroh-gossip.
- [Mesh topologies](./topologies.md) — the network shapes a mesh forms and
  how they hold up.
- [Security & privacy](./security.md) — the threat model and what the protocol
  does and does not protect.
- [Message-history integrity](./history-integrity.md) — per-author signing,
  fork (equivocation) detection, and anti-entropy convergence.
- [FAQ](./faq.md) — frequently asked questions.
