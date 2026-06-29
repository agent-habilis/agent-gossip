---
type: e2e-runbook
title: Todo backends
description: Run a task exchange and judge how it surfaces, with or without a todo plugin installed.
tags: [todo, widget, exchange, fallback, harness]
timestamp: 2026-06-28T00:00:00Z
roles: [initiator, worker]
coordinator: dedicated
harness: pi
prereqs: [ahsw]
network: private
---

# Todo backends

## Scenario

The same [task](/tasks.md) exchange is run to judge how it surfaces in the UI.
Whether a todo plugin is installed is the **human's** choice — the instructions
are identical either way, to mirror real use. The exchange should complete
regardless; the difference is only in how progress is shown: with a plugin it
rides the todo widget, without one it must still be legible in the UI. Targets
pi (where the todo integration lives). Set up per the
[coordinator protocol](/coordinator.md).

## Roles & goals

- **initiator** — delegate a task to the worker and confirm the result.
- **worker** — do the task and return the result.

## Briefing

- swarm: `e2e-todo`
- a small, checkable task (any)
- todo plugin: present or absent — the runner's choice; the brief is the same.

## Expected behavior & UX

- [ ] the task completes end to end regardless of the todo backend (the exchange
      itself is unaffected by it)
- [ ] **with a plugin present:** the exchange is tracked in the todo widget,
      advancing as it progresses to a finished state
- [ ] **without a plugin:** the exchange is still surfaced legibly in the UI as it
      progresses — the user is not left blind just because no todo widget exists
- [ ] how the exchange surfaces in the backend you ran is the thing to judge
