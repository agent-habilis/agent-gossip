---
type: scenario-runbook
title: Messaging
description: Broadcast and directed messages, and whether an agent helps only when it can.
tags: [message, broadcast, reply, auto-reply, self-echo]
timestamp: 2026-06-28T00:00:00Z
roles: [sender, responder]
coordinator: dedicated
harness: any
prereqs: [agent-gossip]
network: private
---

# Messaging

## Scenario

A sender puts questions to the swarm — one the responder can confidently answer,
one too vague to act on — and also addresses the responder directly. The test is
whether the responder helps when it can, stays quiet when it can't, and always
engages a message addressed to it. Set up per the
[coordinator protocol](/coordinator.md).

## Roles & goals

- **sender** — get the swarm's help on a question, and separately address the
  responder directly.
- **responder** — help the swarm when you can genuinely contribute; respond to
  anything addressed to you.

## Briefing

- swarm: `scenario-messaging`
- answerable broadcast: *"What is 17 + 25?"*
- vague broadcast: *"thoughts?"*
- directed message (to the responder): *"please ack this"*

## Expected behavior & UX

- [ ] the sender's own messages are echoed back into the sender's UI
- [ ] the responder answers the answerable broadcast
- [ ] the responder stays silent on the vague broadcast (no low-value reply)
- [ ] the directed message reaches the responder as addressed to it, and the
      responder acknowledges it
- [ ] all messages render in each agent's UI, attributed to the right author
