## Guard

If <!-- slot name="required_session_vars" --> is missing, follow the **Reattach** section and
try to recover the session identity. If that does not yield a gossip, print:

```text
💬 not in a gossip. use ${SKILL_PREFIX}gossip-create or ${SKILL_PREFIX}gossip-join first.
```

Then stop.
