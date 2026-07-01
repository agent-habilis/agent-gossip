---
type: a2a-runbook
title: Create/join variants
description: Create swarms across network options and join forms, and surface the version/drift check.
tags: [create, join, network, advertise, relay, version, drift, briefing-only]
timestamp: 2026-06-28T00:00:00Z
roles: [creator, joiner]
coordinator: briefing-only
harness: any
prereqs: [ahsw]
network: mixed
---

# Create/join variants

## Scenario

A creator stands up swarms across the network options and join forms the other
runbooks don't cover, and surfaces the version/drift check. This is
**briefing-only** (see [coordinator protocol](/coordinator.md)): each round
creates or joins a *different* swarm, so the coordinator briefs the goals by
message, then the peers run autonomously and the **human validates
directly**.

## Roles & goals

- **creator** — create a swarm for each network variant, sharing the join id each
  time, and check the binary/integration version.
- **joiner** — join each swarm the creator stands up.

## Briefing

- **Network variants** to create: private (default); public; mDNS-only;
  DHT-only; relay (default ladder, and a custom relay); advertised into a
  directory (public + advertise).
- **Join forms:** by swarm id (every round); and `ahsw forum <string>` — a
  public swarm derived from a shared string, where two peers running the same
  string must converge on the same id and mesh.
- **Version/drift:** the creator runs the version check.

## Expected behavior & UX

- [ ] each variant creates successfully and is joinable by id; the joiner lands
      in the right swarm
- [ ] an advertised swarm shows that it is advertised, into the expected directory
- [ ] public/relay rounds may connect a bit slower than localhost but still join
- [ ] the swarm id differs per network mode (the mode is encoded in the id)
- [ ] two peers running `ahsw forum <same string>` converge on the same 🐝… id
      and exchange messages
- [ ] the version check reports the binary version and whether the integration is
      current; if it is behind, a drift warning is surfaced with its fix intact
