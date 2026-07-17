## Arguments

The optional first argument is the directory to browse. If omitted, use the
default `global` directory and omit `--directory`.

## Browse

Use the selected adapter (see **Adapters** below) to run:

```bash
agent-gossip discover [--directory DIR]
```

Discovery does not join a room. It only returns advertised room ids.

## Selection

Track `room_found` and `room_lost` events by full `room` id. Present the
live set to the user, preferring entries with more peers. When the user selects
one, stop discovery and invoke `${SKILL_PREFIX}gossip-join $ROOM`.

If no room is found within a bounded wait, print:

```text
💬️ no rooms in `#$DIR` yet
```
