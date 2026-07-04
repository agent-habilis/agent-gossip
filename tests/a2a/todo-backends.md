---
type: a2a-runbook
title: Todo backends
description: Run a task and judge how it surfaces, with or without a todo plugin installed.
tags: [todo, widget, task, fallback, harness]
timestamp: 2026-06-28T00:00:00Z
roles: [initiator, worker]
coordinator: dedicated
harness: pi
prereqs: [agent-gossip]
network: private
---

# Todo backends

## Scenario

The same [task](/tasks.md) is run to judge how it surfaces in the UI.
Whether a todo plugin is installed is the **human's** choice — the instructions
are identical either way, to mirror real use. The task should complete
regardless; the difference is only in how progress is shown: with a plugin it
rides the todo widget, without one it must still be legible in the UI. Targets
pi (where the todo integration lives). Set up per the
[coordinator protocol](/coordinator.md).

## Roles & goals

- **initiator** — delegate a task to the worker and confirm the result.
- **worker** — do the task and return the result.

## Briefing

- swarm: `a2a-todo`
- a small, checkable task (any)
- todo plugin: present or absent — the runner's choice; the brief is the same.

## Expected behavior & UX

- [ ] the task completes end to end regardless of the todo backend (the task
      itself is unaffected by it)
- [ ] **with a plugin present:** the task is tracked in the todo widget,
      advancing as it progresses to a finished state
- [ ] **without a plugin:** the task is still surfaced legibly in the UI as it
      progresses — the user is not left blind just because no todo widget exists
- [ ] how the task surfaces in the backend you ran is the thing to judge
