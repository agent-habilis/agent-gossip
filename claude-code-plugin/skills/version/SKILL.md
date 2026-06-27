---
name: version
description: Show the swarm binary version and whether the installed skill is up to date with it. Use to check for skill drift after upgrading `ahsw`.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just did.
The only output is the command's result block under "Output". Tool
calls are shown by the harness; do not narrate around them.

## What this checks

`ahsw setup` copies the skill onto disk, so upgrading the `ahsw`
binary can leave the installed skill stale — running old instructions
silently. This reports the binary version and, for each agent, whether
its installed skill matches the binary (`up to date` / `out of date`),
with the fix when it doesn't. No swarm or running daemon required.

## Run

```bash
ahsw status
```

## Output

Print the command's output verbatim. If any agent shows `out of date`,
the line already names the fix (`ahsw setup --agent <agent> --execute`)
— surface it as-is; do not paraphrase or act on it without the user.
