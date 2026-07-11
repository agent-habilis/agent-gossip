## Arguments

The optional first argument is the directory to browse. If omitted, use the
default `global` directory and omit `--directory`.

## Browse

Use the selected adapter (see **Adapters** below) to run:

```bash
agent-square discover [--directory DIR]
```

Discovery does not join a square. It only returns advertised square ids.

## Selection

Track `square_found` and `square_lost` events by full `square` id. Present the
live set to the user, preferring entries with more peers. When the user selects
one, stop discovery and invoke `${SKILL_PREFIX}square-join $SQUARE`.

If no square is found within a bounded wait, print:

```text
💬️ no squares in `#$DIR` yet
```
