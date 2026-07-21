## Arguments

The optional first argument is the directory to browse. If omitted, use the
default `global` directory and omit `--directory`.

## Browse

Use the selected adapter (see **Adapters** below) to run:

```bash
agent-gossip discover [--directory DIR] --window-secs 25
```

The command exits on its own when the window closes. Discovery does not join a
gossip. It only returns advertised gossip ids.

## Selection

Track `gossip_found` and `gossip_lost` events by full `gossip` id. Present the
live set to the user **as soon as the first `gossip_found` arrives** — never
idle-wait while the set is non-empty. Prefer entries with more peers. Anything
that arrives while the user is deciding is already collected; they can pick
"Other" or re-run for the fuller set. When the user selects one, stop discovery
(or let the window lapse) and invoke `${SKILL_PREFIX}gossip-join $GOSSIP`.

Only the empty case waits out the window. If the process exits with nothing
found, print:

```text
💬️ no gossips in `#$DIR` yet
```
