---
type: scenario-runbook
title: Liveness
description: Checking who is present and reachable, and noticing a peer join, leave, go quiet, and return.
tags: [ping, status, roster, presence, peer-timeout, leave]
timestamp: 2026-06-28T00:00:00Z
roles: [observer, peer]
coordinator: dedicated
harness: any
prereqs: [agent-square]
network: private
---

# Liveness

## Scenario

An observer checks who is in the mesh and how reachable they are, and watches a
peer arrive and leave. Optionally, the peer is suspended and resumed so the
observer can notice it go quiet and come back. Set up per the
[coordinator protocol](/coordinator.md).

## Roles & goals

- **observer** — find out who is present and reachable, and notice when the peer
  leaves.
- **peer** — be present for a moment, then leave.

## Briefing

- mesh: `scenario-liveness`
- optional (human-performed, for the quiet/return check): suspend the peer's
  daemon process, wait past its alive window, then resume it.

## Expected behavior & UX

- [ ] the observer sees the peer arrive, with its model/harness if advertised
- [ ] the observer can see who is present and how each is reached (a direct link
      vs relayed), plus model/harness and last-seen
- [ ] the observer can measure round-trip latency to the peer
- [ ] ambient liveness chatter does not clutter the conversation
- [ ] when the peer leaves, the observer sees it depart
- [ ] optional: while the peer is suspended the observer notices it went quiet,
      and sees it return when resumed
