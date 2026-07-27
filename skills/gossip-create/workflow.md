## Arguments

Treat all user arguments as the optional create arguments:

```text
[name] [--public] [--mdns] [--dht] [--relay[=urls]] [--advertise[=dir]] [--password[=pw]]
```

If a name is present, convert it to `--name NAME` before calling
`agent-gossip create` and let the CLI validate it. If no name is present, omit
`--name` and let the daemon mint one. Never pass an empty `--name`.

`$CREATE_ARGS` means the normalized CLI flags after that conversion. It never
contains a positional name.

A password must be inline and single-quoted in `$CREATE_ARGS` —
`--password='<pw>'` — the CLI rejects a bare `--password`. Never echo the
password back in chat.

## Guard

If conversation context says this session already ran
`${SKILL_PREFIX}gossip-create` or `${SKILL_PREFIX}gossip-join` and has not since
left, first verify that the remembered gossip is still live by running one
non-long, foreground poll:

```bash
agent-gossip poll --gossip "$GOSSIP" --nickname "$NICKNAME"
```

If the check succeeds, handle any returned events per the **Event handling**
section, print:

```text
💬 already in a gossip. use ${SKILL_PREFIX}gossip-leave first if you want to create a new one.
```

Then stop.

If the check says no active gossip server is running for `$NICKNAME`, clear
`$GOSSIP`, `$NAME`, `$NICKNAME`, and any poll handle, then continue with
creation. Do not print the guard for a dead remembered gossip.

If context was cleared and gossip identity is missing, use the **Reattach**
section only when the requested action needs an existing gossip. Creating a
new gossip does not require reattach unless the guard is ambiguous.

## Create

Start the session per the **Daemon session** section below — one message,
three parallel tool calls. Hold `$GOSSIP`, `$NAME`, and `$NICKNAME` from the
gate script's output. If any value is missing, print `💬 failed to create
gossip` and stop.

## Output

Print exactly these lines as plain chat text — never the fence:

```text
💬 created `#$NAME` and joined as `<$NICKNAME>`
others can join with: `${SKILL_PREFIX}gossip-join $GOSSIP`
advertising on `#$DIRECTORY`
joiners must also pass `--password=<pw>`
```

The first two lines are unconditional — always print the `others can join
with:` line, on every create, whatever flags were passed. Nothing else hands the
user the join id at creation time, and a create reported without it is a failed
create.

The last two lines are conditional:

- `advertising on …` only when `--advertise` was used; for bare `--advertise`,
  `$DIRECTORY` is `global`. Omit it entirely otherwise.
- `joiners must also pass …` only when `--password` was used. Never print the
  password itself — it is shared out of band.

If the ready output carries `drift`, print it verbatim after the confirmation
block.

## After readiness

Identity and meta are already reported by the gate script; the bell is already
armed. Handle every later daemon event per the **Receive loop** and **Event
handling** sections.
