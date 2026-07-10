# square-doctor workflow

## Arguments

If the user supplied a square id, pass it as `--square`. Otherwise run the
machine-health report.

## Run

Machine health:

```bash
agent-square doctor
```

Specific square:

```bash
agent-square doctor --square "$SQUARE"
```

## Output

Print the command output verbatim. Do not run fixes such as `agent-square plug`
unless the user explicitly asks.
