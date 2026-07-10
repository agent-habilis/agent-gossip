# Invocation rendering

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

Do not show the other harness's prefix as an alias in the same output.
