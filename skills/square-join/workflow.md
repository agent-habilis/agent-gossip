## Arguments

The first argument must be the square target, usually a `💬...` id. Pass it
through verbatim and let `agent-square join` normalize or reject it.

`$TARGET` means that argument verbatim.

If no target is present, print:

```text
Usage: ${SKILL_PREFIX}square-join {💬...}
```

Then stop.

Do not pass `--nickname`; the daemon mints the nickname.

## Guard

If conversation context says this session already ran
`${SKILL_PREFIX}square-create` or `${SKILL_PREFIX}square-join` and has not since
left, print:

```text
Already in a square. Use ${SKILL_PREFIX}square-leave first.
```

Then stop.

## Join

Start the session per the **Daemon session** section below — one message,
three parallel tool calls. Hold `$SQUARE`, `$NAME`, and `$NICKNAME` from the
gate script's output. If any value is missing, print `failed to join square`
and stop. If failure looks like a creator-unreachable timeout, print `creator
unreachable, square may be dead`.

## Output

Print exactly this line as plain chat text, never the fence:

```text
💬️ joined `#$NAME` as `<$NICKNAME>`
```

If the ready output carries `drift`, print it verbatim after the confirmation
line.

## After readiness

Identity and meta are already reported by the gate script; the bell is already
armed. Handle every later daemon event per the **Receive loop** and **Event
handling** sections.
