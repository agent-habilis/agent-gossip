---
type: e2e-protocol
title: Coordinator protocol
description: How a dedicated coordinator peer resets state, distributes each role's goal, and observes.
tags: [e2e, coordinator, orchestration, shared-state, protocol]
timestamp: 2026-06-28T00:00:00Z
---

# Coordinator protocol

Every runbook in this [bundle](/index.md) is set up by **one dedicated
coordinator peer**. The coordinator does three things and nothing more: **brief**
each role's goal and the scenario by **swarm message**, **observe**, and — only
when the scenario uses shared state — **reset** that state first. It then leaves
the agents to run, and the **human validates behavior + UX**.

Everything is driven over the swarm itself — messages, and shared state only
where a scenario needs it — never local files, so the coordinator and each role
session can run on **different machines**. For a cross-machine run, create the
swarm `--public` (optionally `--advertise` so peers find it via `ahsw discover`);
peers join by swarm id or discovery.

The coordinator **never plays a scenario role**, and — critically — it **never
tells an agent *how* to behave**. Each agent derives its behavior from its own
skills; whether it derives the right behavior and good UX is exactly what the
test measures. Prescribing the method (which tool to call, push vs. poll,
"register then ack") would defeat the test.

## 1. Reset shared state (only when the scenario uses it)

Most scenarios are driven entirely by messages and need no shared state. When a
scenario *does* use shared state (e.g. a game board the players mutate), it
persists per swarm and accumulates stale keys across runs, so the coordinator
wipes it first: read the document and `remove` every top-level key in one atomic
patch so it starts from `{}`. This is **harness setup**, not agent behavior.
Always reset before briefing a new stateful scenario.

## 2. Brief the *what* — by message

The coordinator briefs the scenario over **swarm messages**: it announces each
role's **goal** and the scenario's **data** (the runbook's *Briefing* section) so
every agent learns its assignment and all are aligned on the same scenario. A
broadcast for the shared rules, a directed message per role for "you are
player-a" — whatever the coordinator judges; the point is the briefing travels
over the swarm as messages, not through a file or a pre-seeded state key.

- **roles** — map the joined nicks to the runbook's roles (in join order, or any
  order the runbook fixes), announced by message.
- **goals** — **what success looks like** for each role; never how to do it.
- **data** — what the agents need (a game's document model, a win rule, a swarm
  name, an ordering constraint). No tool names, no transport, no steps.

The coordinator sends this briefing and does not hand-hold after. Any live
scenario data the agents themselves create lives in **shared state** (e.g.
`/board` for a game — see §1), not in a coordinator-owned key.

## 3. Observe

The coordinator is a swarm member, so it sees the same events the peers do. It
watches the run and reports what it observed. It does **not** sequence phases,
require acks, or nudge the agents — the scenario runs on its own from the goals.
The **human validates behavior + UX**.

## Single-swarm assumption & briefing-only scenarios

A session is a member of one swarm at a time, so scenarios that need a peer to
create or join a *different* swarm can't be observed from the control swarm.
Those are marked `coordinator: briefing-only`: the coordinator briefs the goals
by message, then stops — the peers run autonomously and the
**human validates directly**. Only [discover](/discover.md) and
[create-join-variants](/create-join-variants.md) are briefing-only; everything
else is fully observed in the one swarm (including
[cross-harness](/cross-harness.md), where the harness mix is the only
difference).

## What the coordinator reports vs. what the human validates

- **Coordinator report:** what it observed on the wire — presence, messages,
  shared-state convergence, whether the scenario reached its goal.
- **Human validates:** behavior + UX that only show on a peer's screen — did the
  agent use its skills well, did text flow in the UI, was the experience good.
  This is the real verdict; the coordinator's report is a first pass.

If an agent behaves badly (improvises a poll loop, a helper script, raw CLI
where a skill exists), that is a **finding about the skills**, recorded as-is —
never corrected by adding method to a runbook.
