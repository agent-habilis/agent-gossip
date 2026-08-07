<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP`, `$NAME`, or `$NICKNAME`" -->

## Read

Run:

```bash
agent-gossip meta get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

If the command fails or returns `ok:false`, print `💬 could not read the meta
channel.` and stop.

## Output

Print:

````text
💬 `#$NAME` · meta

```json
$DOCUMENT_PRETTY_JSON
```
````

Pretty-print the `document` value with two-space indentation. Print `{}` for an
empty document.

The response also carries `absent`: the `/peers` entries with no active member
behind them. Only a peer can retract its own entry, so one that died without
leaving gracefully stays in the document indefinitely, still reporting whatever
status it last set — the document is a record of who has been here, not of who
is here now. When `absent` is non-empty, add one line after the JSON so a
reader is not misled by those entries:

```text
💬 not currently active: `<nick-a>`, `<nick-b>`
```

Omit the line when `absent` is empty.
