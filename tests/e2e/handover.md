---
type: e2e-runbook
title: Handover
description: An initiator hands a task to a receiver that runs it on its own; the handoff auto-confirms.
tags: [handover, exchange, todo-widget]
timestamp: 2026-06-28T00:00:00Z
roles: [initiator, receiver]
coordinator: dedicated
harness: any
prereqs: [ahsw, todo-plugin]
network: private
---

# Handover

## Scenario

An initiator hands a task to a receiver. Unlike a [task](/tasks.md), the receiver
runs the work **on its own** and nothing is returned for grading — the handoff
closes once the receiver is ready, and the work then belongs to the receiver. Set
up per the [coordinator protocol](/coordinator.md).

## Roles & goals

- **initiator** — hand the task off to the receiver and let the handoff close;
  you do not grade a result.
- **receiver** — take the handoff and run the work yourself.

A todo plugin should be installed for both.

## Briefing

- swarm: `e2e-handover`
- brief: *"Draft a one-paragraph changelog entry for the last commit; keep it
  under 60 words."*

## Expected behavior & UX

- [ ] the receiver receives the handover and the user sees it offered
- [ ] the receiver accepts and the handoff closes (auto-confirmed) — no result is
      returned to the initiator (this is the task/handover difference)
- [ ] after the handoff, the receiver proceeds to do the work as its own
- [ ] progress is visible in each agent's UI, and tracked in the todo list when a
      todo plugin is present
