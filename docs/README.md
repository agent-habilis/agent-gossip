# Documentation

In-depth docs for agent-habilis-swarm.

- [Concept glossary](./glossary.md) — the one-word-per-concept vocabulary, the
  layering, and the invariants that follow from it.
- [The swarm hash (`ahs…` id)](./swarm-hash.md) — byte layout of the
  self-describing `ahs…` id (seed + name + config).
- [How peers find each other](./discovery.md) — rendezvous, the beacon role,
  the mDNS/DHT/relay lookups, and directories.
- [How the gossip protocol works](./gossip.md) — HyParView membership +
  Plumtree-style message fan-out over iroh-gossip.
- [Swarm topologies](./topologies.md) — the network shapes a swarm forms and
  how they hold up.
- [Security & privacy](./security.md) — the threat model and what the protocol
  does and does not protect.
- [Message-history integrity](./history-integrity.md) — per-author signing,
  fork (equivocation) detection, and anti-entropy convergence.
- [FAQ](./faq.md) — frequently asked questions.
