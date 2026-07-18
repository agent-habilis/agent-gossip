## Guard

If `$GOSSIP`, `$NAME`, or `$NICKNAME` is missing, follow the **Reattach**
section and try to recover the session identity. If that does not yield a
gossip, print:

```text
💬 Not in a gossip. Use ${SKILL_PREFIX}gossip-create or ${SKILL_PREFIX}gossip-join first.
```

Then stop.

## Read

Run:

```bash
agent-gossip state get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

If the command fails or returns `ok:false`, print `💬 Could not read shared
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
