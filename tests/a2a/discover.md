---
type: a2a-runbook
title: Discover
description: A host makes a swarm discoverable in a directory; a discoverer finds and joins it.
tags: [discover, advertise, directory, briefing-only]
timestamp: 2026-06-28T00:00:00Z
roles: [host, discoverer]
coordinator: briefing-only
harness: any
prereqs: [agent-gossip]
network: public
---

# Discover

## Scenario

A host makes its swarm discoverable in a directory; a discoverer browses that
directory and joins the swarm. This is a **briefing-only** scenario (see
[coordinator protocol](/coordinator.md)): the discoverer must be *outside* the
host's swarm and the host advertises a *new* swarm, so the coordinator briefs
the goals by message, then the peers run autonomously and the **human
validates directly**.

## Roles & goals

- **host** — make your swarm discoverable in the directory.
- **discoverer** — find the swarm in the directory and join it.

## Briefing

- directory: `a2a-dir` (named, to avoid the noisy global directory)
- swarm: `a2a-discover`
- ordering: the discoverer should look at the directory **before** the host
  advertises (so it first sees the empty state).

## Expected behavior & UX

- [ ] the discoverer first sees the directory empty (no swarms yet)
- [ ] after the host advertises, the discoverer sees the swarm appear in the
      directory
- [ ] the discoverer joins it and the host sees the discoverer arrive
- [ ] the discovery flow is legible in the discoverer's UI (browsing, the empty
      state, the swarm appearing, the join)
