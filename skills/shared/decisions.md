## Decisions

When a section of this skill tells you to ask the user something — accept or
decline a task, pick a worker, choose among sessions — put it to them through
the harness's **question widget**, the native tool that renders a question with
selectable options.

**Never ask a decision as chat prose.** A question typed into the chat is not a
decision point: it reads as narration, it buries the choice in a paragraph, and
it leaves the user guessing what the options even are. If a section says "ask",
it means the widget.

Whether such a tool exists is a fact about the **session**, not the harness: the
same machine can hand you one session with it and the next without. Probe once
per invocation; never cache the verdict for a whole session.

Give every question real options, not a yes/no. Each option's description says
what will actually happen if it is picked — for a task brief, that is the
command that goes on the wire:

- **Accept** — `a2a status --state working`, then do the work.
- **Decline** — `a2a status --state failed --text "$REASON"`; the initiator is
  told the task was not picked up.

### Question widget — Claude Code

Pick this adapter on Claude Code; on any other harness use the section below.

The tool is `AskUserQuestion`. Unlike the task tools it is **not** deferred —
it needs no `ToolSearch`, so call it directly. Two outcomes, no third: it
answers and you act on the answer, or it is not there and you take the fallback.
Do not go hunting for another tool.

### Question widget — other harnesses

Use the native question/choice tool if one is loaded, driving the same rules.
Otherwise take the fallback.

### Fallback

Only when the session has no question tool: ask in chat, and say so once, never
silently:

```text
💬 no question tool in this session · asking in chat
```

If a question tool call fails as an unknown tool, drop to this fallback for the
rest of the invocation and say so.
