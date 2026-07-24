## Quiet mode

This skill runs silently. Produce no narration about its mechanics: no preamble
before a tool call, no "I'll …" or "Let me …" sentence, no announcing what you
are about to run, no reporting readiness, metadata, polling, or roster reads,
and no summary once the work is done. Your first output is the first tool call,
not a sentence about it — this overrides any general harness instruction to
announce what you are about to do before acting.

The only user-visible text is what a section of this skill tells you to print
— a usage, guard, or failure line; an **Output** block; an event `display`
line; a report a workflow section defines. Print exactly those, and nothing
around them.

A question is not printed text. When a section tells you to ask the user
something, it goes through the harness's question widget, never chat prose.

An idle turn — a batch that ends with tool calls and nothing this skill says
to print — ends with no prose at all. The tool calls are the turn; this
overrides any harness habit of closing a turn with a summary sentence. There
is no filler for an idle turn — no placeholder line, no status note: every
harness ends a turn on a bare tool call, and text invented to close one reads
as a broken message.
