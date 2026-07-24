## Command prefix

Render user-facing skill commands with the current harness's command prefix.

- Codex: `$gossip-*`
- Pi: `/skill:gossip-*`
- All other harnesses: `/gossip-*`

Hold `$SKILL_PREFIX` as `$` for Codex, `/skill:` for Pi, and `/` otherwise. When printing usage,
guards, or next-step instructions, render commands as:

```text
${SKILL_PREFIX}gossip-create
${SKILL_PREFIX}gossip-join
${SKILL_PREFIX}gossip-leave
```

Do not show the other harness's prefix as an alias in the same output. The
harness is the product hosting the agent, not the model vendor — never guess
it from the model name.

## Placeholder notation

Inside a command you are about to run, two spellings do two different jobs.
Keep them apart — a value that reads as something to reason about buys a round
of deliberation you then narrate.

- `$NAME` — a value you already hold, from the arguments or from a command's
  output. Splice it in; there is nothing to work out.
- `{NAME}` — a value only you can supply, from what you know about your own
  runtime (`{MODEL}`, `{HARNESS}`). Resolve it before issuing the command.

Braces in a line you *print* are literal: the `{💬...}` and `{text}` in a usage
line are part of the message, not a substitution.

## Output rendering

Every user-facing line a workflow tells you to print — fenced `text`
templates and event `display` strings — is chat markdown. Emit the lines
bare: never wrap them in a code fence and never backslash-escape the
backticks. A fence around a template only delimits it inside this document;
it is not part of the output.

In any `💬` line you compose beyond these templates, separate fields with
` · ` — never a hyphen or dash.
