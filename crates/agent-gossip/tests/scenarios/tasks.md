---
type: scenario-runbook
title: Tasks
description: An initiator delegates a task to a worker, who does it and returns a result the initiator confirms.
tags: [task, todo-widget, result]
timestamp: 2026-06-28T00:00:00Z
roles: [initiator, worker]
coordinator: dedicated
harness: any
prereqs: [agent-gossip, todo-plugin]
network: private
---

# Tasks

## Scenario

An initiator delegates a small, checkable task to a worker. The worker does the
work and returns a result. The initiator reviews it and confirms (or asks for a
revision — that path is in [task edge cases](/task-edge-cases.md)). Set
up per the [coordinator protocol](/coordinator.md).

## Roles & goals

- **initiator** — delegate the task to the worker and confirm the returned
  result once it meets the criterion.
- **worker** — do the delegated task and return the result.

A todo plugin should be installed for both (how the task surfaces with or
without one is [todo-backends](/todo-backends.md)).

## Briefing

- mesh: `scenario-tasks`
- task: *"Sum the integers from 1 to 100 inclusive; reply with the single
  integer."* (small, self-contained, verifiable = 5050; no local files, so it
  holds across machines)

## Expected behavior & UX

- [ ] the worker receives the delegated task and the user sees it offered
- [ ] the worker completes it and returns a result; the initiator sees that
      result attributed to the worker
- [ ] the initiator confirms; the task closes cleanly on both sides
- [ ] the task's progress is visible in each agent's UI, and tracked in the
      todo list when a todo plugin is present
- [ ] the result is correct for the task (the integer, 5050)
