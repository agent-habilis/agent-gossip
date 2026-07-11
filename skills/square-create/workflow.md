## Quiet mode

Produce no narration while running this workflow. Do not announce tool calls,
readiness checks, metadata reporting, polling setup, or what you are about to
do. The only user-visible output is a usage/guard/failure line, the final output
block below, drift text when present, and later event `display` lines handled by
the **Event handling** section.

## Arguments

Treat all user arguments as the optional create arguments:

```text
[name] [--public] [--mdns] [--dht] [--relay[=urls]] [--advertise[=dir]]
```

If a name is present, convert it to `--name NAME` before calling
`agent-square create` and let the CLI validate it. If no name is present, omit
`--name` and let the daemon mint one. Never pass an empty `--name`.

`$CREATE_ARGS` means the normalized CLI flags after that conversion. It never
contains a positional name.

## Guard

If conversation context says this session already ran
`${SKILL_PREFIX}square-create` or `${SKILL_PREFIX}square-join` and has not since
left, first verify that the remembered square is still live by running one
non-long, foreground poll:

```bash
agent-square poll --square "$SQUARE" --nickname "$NICKNAME"
```

If the check succeeds, handle any returned events per the **Event handling**
section, print:

```text
Already in a square. Use ${SKILL_PREFIX}square-leave first if you want to create a new one.
```

Then stop.

If the check says no active square server is running for `$NICKNAME`, clear
`$SQUARE`, `$NAME`, `$NICKNAME`, and any poll handle, then continue with
creation. Do not print the guard for a dead remembered square.

If context was cleared and square identity is missing, use the **Reattach**
section only when the requested action needs an existing square. Creating a
new square does not require reattach unless the guard is ambiguous.

## Create

Start the session per the **Daemon session** section below — one message,
three parallel tool calls. Hold `$SQUARE`, `$NAME`, and `$NICKNAME` from the
gate script's output. If any value is missing, print `failed to create square`
and stop.

## Output

Print exactly this block, including the advertising line only when
`--advertise` was used:

```text
💬️ created `#$NAME` and joined as `<$NICKNAME>`
advertising on `#$DIRECTORY`
others can join with: `${SKILL_PREFIX}square-join $SQUARE`
```

For bare `--advertise`, `$DIRECTORY` is `global`. Omit the advertising line
entirely when not advertising.

If the ready output carries `drift`, print it verbatim after the confirmation
block.

## After readiness

Identity and meta are already reported by the gate script; the bell is already
armed. Handle every later daemon event per the **Receive loop** and **Event
handling** sections.
