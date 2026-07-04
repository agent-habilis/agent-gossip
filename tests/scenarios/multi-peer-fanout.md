---
type: scenario-runbook
title: Multi-peer fan-out
description: An initiator delegates a distinct task to each of two workers; results return independently.
tags: [task, multi-peer, fan-out]
timestamp: 2026-06-28T00:00:00Z
roles: [initiator, worker-1, worker-2]
coordinator: dedicated
harness: any
prereqs: [agent-gossip, todo-plugin]
network: private
---

# Multi-peer fan-out

## Scenario

An initiator delegates a different task to each of two workers at the same time
and tracks both. The results come back independently, in whatever order. Set up
per the [coordinator protocol](/coordinator.md).

## Roles & goals

- **initiator** — delegate a distinct task to each of the two workers and confirm
  each result as it returns.
- **worker-1**, **worker-2** — each does its own task and returns the result.

A todo plugin should be installed for the initiator and both workers.

## Briefing

- swarm: `scenario-fanout`
- task for worker-1 and a different task for worker-2 (any two small, checkable
  tasks)

## Expected behavior & UX

- [ ] each worker receives only its own task, not the other's
- [ ] both results come back, each attributed to its worker; order is not assumed
- [ ] the initiator confirms each; both tasks close cleanly
- [ ] the initiator tracks both tasks at once (two todo items when a plugin
      is present), each progressing independently
