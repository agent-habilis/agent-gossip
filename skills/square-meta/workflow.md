## Guard

If `$SQUARE`, `$NAME`, or `$NICKNAME` is missing, follow the **Reattach**
section and try to recover the session identity. If that does not yield a
square, print:

```text
💬 Not in a square. Use ${SKILL_PREFIX}square-create or ${SKILL_PREFIX}square-join first.
```

Then stop.

## Read

Run:

```bash
agent-square meta get --square "$SQUARE" --nickname "$NICKNAME"
```

If the command fails or returns `ok:false`, print `💬 Could not read the meta
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
