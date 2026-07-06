---
type: scenario-runbook
title: Task edge cases
description: The non-happy-path task outcomes — decline, cancel, clarifying questions, and revision.
tags: [task, decline, cancel, context, revision]
timestamp: 2026-06-28T00:00:00Z
roles: [initiator, receiver]
coordinator: dedicated
harness: any
prereqs: [agent-square, todo-plugin]
network: private
---

# Task edge cases

## Scenario

The [tasks](/tasks.md) and [handover](/handover.md) runbooks cover the happy
path; this puts an initiator and receiver through the outcomes that aren't a
clean accept-and-finish: an offer that gets **declined**, one the initiator
**cancels**, one that's **under-specified** so the receiver must ask, and one
whose first result the initiator sends back for a **revision**. Set up per the
[coordinator protocol](/coordinator.md).

## Roles & goals

- **initiator** — run four tasks with the receiver: one you cancel after
  offering; one whose brief is vague (expect questions); one whose first result
  you reject and ask to be redone; and one the receiver will turn down. See each
  to a clean close.
- **receiver** — handle each task as it comes: turn down what you shouldn't
  take, ask when the brief is unclear, deliver and then revise when asked.

A todo plugin should be installed for both.

## Briefing

- mesh: `scenario-task-edges`
- the four tasks (any small tasks; the point is the outcome):
  - **decline:** an offer the receiver is expected to turn down.
  - **cancel:** an offer the initiator withdraws before it completes.
  - **clarify:** a deliberately under-specified brief (e.g. *"summarize the
    doc"* with no document named).
  - **revision:** a brief with a strict criterion the first result misses, so
    the initiator asks for a redo.

## Expected behavior & UX

- [ ] **decline:** the receiver turns the offer down; both sides close it; no
      dangling state
- [ ] **cancel:** the receiver sees the offer withdrawn; both sides clear it
- [ ] **clarify:** the receiver's question reaches the initiator, the answer
      comes back, and the task then completes
- [ ] **revision:** the initiator's request for a redo reaches the receiver, the
      receiver revises, and the initiator confirms the revised result
- [ ] each outcome is visible in both UIs; todo items (when a plugin is present)
      end in their correct terminal state, never stuck mid-flight
