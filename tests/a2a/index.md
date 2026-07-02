---
type: a2a-index
title: Swarm a2a runbooks
description: Coordinator-set-up agent-to-agent runbooks that validate swarm behavior and UX, judged by a human.
tags: [a2a, swarm, runbook, coordinator, okf]
timestamp: 2026-06-28T00:00:00Z
---

# Swarm a2a runbooks

This is an [OKF](https://okf.md/) bundle of agent-to-agent runbooks for the
agent-habilis swarm. A **dedicated coordinator** peer briefs each role's **goal**
+ the scenario by **swarm message** (resetting shared state only when a scenario
uses it), then observes. The agents then run using **their own skills** — and a
**human validates the behavior and UX**.

Each runbook says **what** to test, never **how** an agent should do it. Whether
an agent derives the right behavior and good UX from its skills is the thing
under test; if it improvises badly, that is a **finding about the skills**, not a
runbook to patch. See the [coordinator protocol](/coordinator.md).

The runbooks are harness-agnostic — they describe goals, not commands.

## Start here

- [Coordinator protocol](/coordinator.md) — reset, distribute, observe.

## Runbooks

Core:
- [Tasks](/tasks.md) — delegate a task to a peer, get the result, confirm it.
- [Handover](/handover.md) — hand a task to a peer that runs it on its own.
- [Shared state — Connect Four](/state-connect-four.md) — a turn-based game over
  the shared-state document.
- [Shared state — Nim "21"](/state-nim-21.md) — a fast turn-based game over the
  shared-state document (converges in ~6–12 moves).
- [Discover](/discover.md) — advertise a swarm and find it from a directory.

Coverage:
- [Messaging](/messaging.md) — broadcast, the auto-reply judgement, directed reply.
- [Liveness](/liveness.md) — ping, status/roster, presence, quiet/return, leave.
- [Task edge cases](/task-edge-cases.md) — decline, cancel, context Q&A,
  task revision.
- [Todo backends](/todo-backends.md) — task tracking with vs without a todo
  plugin.
- [Multi-peer fan-out](/multi-peer-fanout.md) — one coordinator, two workers.
- [Cross-harness](/cross-harness.md) — pi ↔ Claude Code in one swarm.
- [Create/join variants](/create-join-variants.md) — network flags, join forms,
  version/drift.

## How to run

1. **Prerequisites:** the `ahsw` binary on `PATH`. For the todo cases, a todo
   plugin installed in the role peers. For [cross-harness](/cross-harness.md),
   one pi and one Claude Code session.
2. Open **one coordinator session + one per role** (the runbook's `roles:`
   frontmatter). Each is a real, independent swarm member — and may run on a
   **different machine** (create the swarm `--public`, join by id or
   `ahsw discover`).
3. The coordinator **briefs** each role's goal + the scenario by **swarm
   message**, then observes. The agents run on their own. Nothing travels
   through local files.
4. **If the scenario uses shared state** (e.g. a game board), the coordinator
   resets it to `{}` first — state persists per swarm, so always start fresh.
   See [coordinator.md](/coordinator.md).
5. **Validate** the runbook's **Expected behavior & UX** by eye. The
   coordinator's report is a first pass; your UI judgement is the verdict.
   Nothing is committed.

## Reading a runbook

Each runbook has four parts:
- **Scenario** — the situation.
- **Roles & goals** — what success looks like for each role (a goal, not a
  method).
- **Briefing** — the data the coordinator broadcasts by message (swarm name,
  document model, rules, ordering). Never a tool or step.
- **Expected behavior & UX** — the observable outcomes/experience to validate.

Expected UI strings reference the canonical `display` lines the daemon emits
(`src/output/json.rs` `*_display`) and the front-end Output sections
(`claude-code-plugin/skills/*/SKILL.md`). The bee is `🐝️` (U+FE0F); a swarm id
keeps a bare `🐝` prefix.

## Capability reference (for the human, not the agents)

When you observe an action and want to know its surface, this maps a capability
to each harness. Runbooks never cite these — agents derive them from their
skills. This table is only to help you read what happened.

| capability | pi command | pi tool | Claude Code skill |
|---|---|---|---|
| create | `/swarm-create` | `swarm_create` | `/swarm:create` |
| join | `/swarm-join` | `swarm_join` | `/swarm:join` |
| discover | `/swarm-discover` | `swarm_discover` | `/swarm:discover` |
| broadcast | `/swarm-msg` | `swarm_send` | `/swarm:msg` |
| directed reply | `/swarm-reply` | `swarm_send` (with reply) | `/swarm:reply` |
| handover | `/swarm-handover` | `swarm_handover` | `/swarm:handover` |
| task | `/swarm-task` | `swarm_task` | `/swarm:task` |
| advance task leg | — | `swarm_task_leg` | (skill drives the legs) |
| status / roster | `/swarm-status` | `swarm_status` | `/swarm:status` |
| ping | `/swarm-ping` | `swarm_ping` | `/swarm:ping` |
| read state | `/swarm-state` | `swarm_get_state` | `ahsw state get` |
| merge state | `/swarm-state-merge` | `swarm_apply_merge` | `ahsw state merge` |
| leave | `/swarm-leave` | `swarm_leave` | `/swarm:leave` |
| version / drift | `/swarm-version` | — | `/swarm:version` |
