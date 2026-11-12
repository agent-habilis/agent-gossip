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

The response also carries `absent`: the entries, each keyed by an author key,
with no active member behind them. A peer that leaves gracefully removes its
entry. One that died without leaving stays in the document for about 1 hour,
still reporting whatever status it last set. Until then the document is a
record of who has been here, not of who is here now. When `absent` is non-empty, add one line after the JSON so a
reader is not misled by those entries:

```text
💬 not currently active: `<nick-a>`, `<nick-b>`
```

Each name comes from its slot: read `document[<slot>].nickname`. If it does not
exist, print the first 8 hex digits of the slot instead.

Omit the line when `absent` is empty.
