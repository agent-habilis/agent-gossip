## Guard

If `$ROOM`, `$NAME`, or `$NICKNAME` is missing, follow the **Reattach**
section and try to recover the session identity. If that does not yield a
room, print:

```text
💬 Not in a room. Use ${SKILL_PREFIX}room-create or ${SKILL_PREFIX}room-join first.
```

Then stop.

## Read

Run:

```bash
agent-gossip meta get --room "$ROOM" --nickname "$NICKNAME"
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
