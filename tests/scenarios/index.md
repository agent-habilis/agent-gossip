---
type: scenario-index
title: Mesh scenario runbooks
description: Coordinator-set-up agent-to-agent runbooks that validate mesh behavior and UX, judged by a human.
tags: [scenario, mesh, runbook, coordinator, okf]
timestamp: 2026-06-28T00:00:00Z
---

# Mesh scenario runbooks

This is an [OKF](https://okf.md/) bundle of agent-to-agent runbooks for the
agent-habilis mesh. A **dedicated coordinator** peer briefs each role's **goal**
+ the scenario by **mesh message** (resetting shared state only when a scenario
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
- [Discover](/discover.md) — advertise a mesh and find it from a directory.

Coverage:
- [Messaging](/messaging.md) — broadcast, the auto-reply judgement, directed reply.
- [Liveness](/liveness.md) — ping, status/roster, presence, quiet/return, leave.
- [Task edge cases](/task-edge-cases.md) — decline, cancel, context Q&A,
  task revision.
- [Todo backends](/todo-backends.md) — task tracking with vs without a todo
  plugin.
- [Multi-peer fan-out](/multi-peer-fanout.md) — one coordinator, two workers.
- [Cross-harness](/cross-harness.md) — pi ↔ Claude Code in one mesh.
- [Create/join variants](/create-join-variants.md) — network flags, join forms,
  version/drift.

## How to run

1. **Prerequisites:** the `agent-mesh` binary on `PATH`. For the todo cases, a todo
   plugin installed in the role peers. For [cross-harness](/cross-harness.md),
   one pi and one Claude Code session.
2. Open **one coordinator session + one per role** (the runbook's `roles:`
   frontmatter). Each is a real, independent mesh member — and may run on a
   **different machine** (create the mesh `--public`, join by id or
   `agent-mesh discover`).
3. The coordinator **briefs** each role's goal + the scenario by **mesh
   message**, then observes. The agents run on their own. Nothing travels
   through local files.
4. **If the scenario uses shared state** (e.g. a game board), the coordinator
   resets it to `{}` first — state persists per mesh, so always start fresh.
   See [coordinator.md](/coordinator.md).
5. **Validate** the runbook's **Expected behavior & UX** by eye. The
   coordinator's report is a first pass; your UI judgement is the verdict.
   Nothing is committed.

## Reading a runbook

Each runbook has four parts:
- **Scenario** — the situation.
- **Roles & goals** — what success looks like for each role (a goal, not a
  method).
- **Briefing** — the data the coordinator broadcasts by message (mesh name,
  document model, rules, ordering). Never a tool or step.
- **Expected behavior & UX** — the observable outcomes/experience to validate.

Expected UI strings reference the canonical `display` lines the daemon emits
(`src/output/json.rs` `*_display`) and the front-end Output sections
(`claude-code-plugin/skills/*/SKILL.md`). The bee is `💬️` (U+FE0F); a mesh id
is canonically `💬://<base58>`.

## Capability reference (for the human, not the agents)

When you observe an action and want to know its surface, this maps a capability
to each harness. Runbooks never cite these — agents derive them from their
skills. This table is only to help you read what happened.

| capability | pi command | pi tool | Claude Code skill |
|---|---|---|---|
| create | `/mesh-create` | `mesh_create` | `/mesh:create` |
| join | `/mesh-join` | `mesh_join` | `/mesh:join` |
| discover | `/mesh-discover` | `mesh_discover` | `/mesh:discover` |
| broadcast | `/mesh-msg` | `mesh_send` | `/mesh:msg` |
| directed reply | `/mesh-reply` | `mesh_send` (with reply) | `/mesh:reply` |
| handover | `/mesh-handover` | `mesh_handover` | `/mesh:handover` |
| task | `/mesh-task` | `mesh_task` | `/mesh:task` |
| advance a task leg | — | `mesh_advance` | (skill drives the legs) |
| status / roster | `/mesh-status` | `mesh_status` | `/mesh:status` |
| ping | `/mesh-ping` | `mesh_ping` | `/mesh:ping` |
| read state | `/mesh-state` | `mesh_get_state` | `agent-mesh state get` |
| merge state | `/mesh-state-merge` | `mesh_apply_merge` | `agent-mesh state merge` |
| leave | `/mesh-leave` | `mesh_leave` | `/mesh:leave` |
| version / drift | `/mesh-version` | — | `/mesh:version` |
