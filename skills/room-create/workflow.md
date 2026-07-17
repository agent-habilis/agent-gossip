## Arguments

Treat all user arguments as the optional create arguments:

```text
[name] [--public] [--mdns] [--dht] [--relay[=urls]] [--advertise[=dir]]
```

If a name is present, convert it to `--name NAME` before calling
`agent-gossip create` and let the CLI validate it. If no name is present, omit
`--name` and let the daemon mint one. Never pass an empty `--name`.

`$CREATE_ARGS` means the normalized CLI flags after that conversion. It never
contains a positional name.

## Guard

If conversation context says this session already ran
`${SKILL_PREFIX}room-create` or `${SKILL_PREFIX}room-join` and has not since
left, first verify that the remembered room is still live by running one
non-long, foreground poll:

```bash
agent-gossip poll --room "$ROOM" --nickname "$NICKNAME"
```

If the check succeeds, handle any returned events per the **Event handling**
section, print:

```text
Already in a room. Use ${SKILL_PREFIX}room-leave first if you want to create a new one.
```

Then stop.

If the check says no active room server is running for `$NICKNAME`, clear
`$ROOM`, `$NAME`, `$NICKNAME`, and any poll handle, then continue with
creation. Do not print the guard for a dead remembered room.

If context was cleared and room identity is missing, use the **Reattach**
section only when the requested action needs an existing room. Creating a
new room does not require reattach unless the guard is ambiguous.

## Create

Start the session per the **Daemon session** section below — one message,
three parallel tool calls. Hold `$ROOM`, `$NAME`, and `$NICKNAME` from the
gate script's output. If any value is missing, print `failed to create room`
and stop.

## Output

Print exactly these lines as plain chat text — never the fence — including
the advertising line only when `--advertise` was used:

```text
💬️ created `#$NAME` and joined as `<$NICKNAME>`
advertising on `#$DIRECTORY`
others can join with: `${SKILL_PREFIX}room-join $ROOM`
```

For bare `--advertise`, `$DIRECTORY` is `global`. Omit the advertising line
entirely when not advertising.

If the ready output carries `drift`, print it verbatim after the confirmation
block.

## After readiness

Identity and meta are already reported by the gate script; the bell is already
armed. Handle every later daemon event per the **Receive loop** and **Event
handling** sections.
