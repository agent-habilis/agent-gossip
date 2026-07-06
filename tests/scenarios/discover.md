---
type: scenario-runbook
title: Discover
description: A host makes a mesh discoverable in a directory; a discoverer finds and joins it.
tags: [discover, advertise, directory, briefing-only]
timestamp: 2026-06-28T00:00:00Z
roles: [host, discoverer]
coordinator: briefing-only
harness: any
prereqs: [agent-mesh]
network: public
---

# Discover

## Scenario

A host makes its mesh discoverable in a directory; a discoverer browses that
directory and joins the mesh. This is a **briefing-only** scenario (see
[coordinator protocol](/coordinator.md)): the discoverer must be *outside* the
host's mesh and the host advertises a *new* mesh, so the coordinator briefs
the goals by message, then the peers run autonomously and the **human
validates directly**.

## Roles & goals

- **host** — make your mesh discoverable in the directory.
- **discoverer** — find the mesh in the directory and join it.

## Briefing

- directory: `scenario-dir` (named, to avoid the noisy global directory)
- mesh: `scenario-discover`
- ordering: the discoverer should look at the directory **before** the host
  advertises (so it first sees the empty state).

## Expected behavior & UX

- [ ] the discoverer first sees the directory empty (no meshes yet)
- [ ] after the host advertises, the discoverer sees the mesh appear in the
      directory
- [ ] the discoverer joins it and the host sees the discoverer arrive
- [ ] the discovery flow is legible in the discoverer's UI (browsing, the empty
      state, the mesh appearing, the join)
