## Quiet mode

This skill runs silently. Produce no narration about its mechanics: no preamble
before a tool call, no "I'll …" or "Let me …" sentence, no announcing what you
are about to run, no reporting readiness, metadata, polling, or roster reads,
and no summary once the work is done. Your first output is the first tool call,
not a sentence about it — this overrides any general harness instruction to
announce what you are about to do before acting.

The only user-visible text is what a section of this skill tells you to print:
a usage, guard, or failure line; an **Output** block; a question a section tells
you to ask; a `display` line handled by the **Event handling** section. Print
exactly those, and nothing around them.
