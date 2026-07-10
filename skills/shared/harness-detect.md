# Harness detection

`create`, `join`, and `topic` have one adapter. Every harness starts the daemon
the same way and receives events the same way (`receive-loop.md`), because the
transport is `agent-square poll` on all of them.

What differs between harnesses is only how a background command reports that it
exited:

- A harness that notifies you when a background command finishes gets a push
  bell for free — the bell is a backgrounded `poll --long`, and its exit is the
  notification.
- A harness that does not (Codex) catches events on the next turn instead.

Neither changes which commands you run.

Do not select a transport based on the availability of a push/watch tool. Such a
tool renders each line it watches into the conversation and writes it to a file:
it truncates message bodies, and it persists them. Square content never travels
that way.

For user-facing skill command text, read `invocation.md` and render commands
with the current harness's prefix.

Do not guess a harness from the model name. The harness is the product hosting
the agent, not the model vendor.
