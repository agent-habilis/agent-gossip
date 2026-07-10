# square-state workflow

## Guard

If `$SQUARE`, `$NAME`, or `$NICKNAME` is missing, read `../shared/reattach.md`
and try to recover the session identity. If that does not yield a square, print:

```text
💬 Not in a square. Use ${SKILL_PREFIX}square-create or ${SKILL_PREFIX}square-join first.
```

Then stop.

## Read

Run:

```bash
agent-square state get --square "$SQUARE" --nickname "$NICKNAME"
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
