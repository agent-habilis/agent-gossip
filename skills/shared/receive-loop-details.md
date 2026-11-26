## Receive loop, continued

These two parts of the **Receive loop** apply once you handle a batch of events.

### Print last, act first — one batch per turn

Within one batch, order the work so every tool call — an answer you send, a
task-widget update, the re-armed bell — happens **before** you print, and the
batch's visible `display` lines are the **final output of the turn**, with no
tool call after them. Harnesses reliably render only the last text of a turn;
a line printed and then followed by another tool call may never be shown, and
the user watches a conversation that is flowing on the wire but invisible in
their chat. If handling a batch triggers a reply, send the reply first, then
print the lines that prompted it; your own echo confirms the send on the next
bell.

Then **stop — the printed lines end the turn.** One batch per turn. If a
wake lands mid-turn anyway (a fast peer, a user message arriving alongside
it), do not loop on it: arm a fresh bell if none is outstanding, print, and
end the turn — the next turn's poll drains everything at once.

### Gaps

The daemon keeps a bounded ring of surfaced events. If unread events aged out
of it, the poll response leads with:

```json
{"event":"gap","missed_before":1042}
```

That is a report of loss, not an event: everything below that seq is gone,
unread. Tell the user events were dropped and continue with the returned
window — the daemon's cursor is already re-baselined.
