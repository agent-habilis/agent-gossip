## Arguments

The optional first argument is the directory to browse. If omitted, use the
default `global` directory and omit `--directory`.

## Browse

Use the selected adapter (see **Adapters** below) to run:

```bash
agent-gossip discover [--directory DIR]
```

Discovery does not join a gossip. It only returns advertised gossip ids.

## Selection

Track `gossip_found` and `gossip_lost` events by full `gossip` id. Present the
live set to the user, preferring entries with more peers. When the user selects
one, stop discovery and invoke `${SKILL_PREFIX}gossip-join $GOSSIP`.

If no gossip is found within a bounded wait, print:

```text
💬️ no gossips in `#$DIR` yet
```
