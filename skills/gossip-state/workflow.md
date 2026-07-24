<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP`, `$NAME`, or `$NICKNAME`" -->

## Read

Run:

```bash
agent-gossip state get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

If the command fails or returns `ok:false`, print `💬 could not read shared
state.` and stop.

## Output

Print:

````text
💬 `#$NAME` · state

```json
$DOCUMENT_PRETTY_JSON
```
````

Pretty-print the `document` value with two-space indentation. Print `{}` for an
empty document.
