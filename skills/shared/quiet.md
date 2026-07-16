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
