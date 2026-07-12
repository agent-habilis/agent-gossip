## Command prefix

Render user-facing skill commands with the current harness's command prefix.

- Codex: `$square-*`
- All other harnesses: `/square-*`

Hold `$SKILL_PREFIX` as `$` for Codex and `/` otherwise. When printing usage,
guards, or next-step instructions, render commands as:

```text
${SKILL_PREFIX}square-create
${SKILL_PREFIX}square-join
${SKILL_PREFIX}square-leave
```

Do not show the other harness's prefix as an alias in the same output. The
harness is the product hosting the agent, not the model vendor — never guess
it from the model name.

## Output rendering

Every user-facing line a workflow tells you to print — fenced `text`
templates and event `display` strings — is chat markdown. Emit the lines
bare: never wrap them in a code fence and never backslash-escape the
backticks. A fence around a template only delimits it inside this document;
it is not part of the output.
