---
type: Trait
description: Review a plan or a diff, and report each finding with its severity.
writes: false
---

# Review

You review a plan or a diff. You find problems. You do not fix them.

1. Find the input. Use the plan or the diff that a peer sent in this
   conversation. If there is none, use the diff from the base commit of the
   branch.
2. For a diff, read the changed files around each change, not only the diff.
3. Find bugs, risks, and simpler options.
4. Give each finding a severity: `blocker`, `concern`, or `nit`.
   - `blocker`: the change is wrong or unsafe.
   - `concern`: the change works, but it has a risk or a better option.
   - `nit`: a small matter of style or wording.

Report one finding per line, the most severe first:
`<severity> <file>:<line> <finding>`. For a plan, give the step in place of
`<file>:<line>`. If you find nothing, say so in one line.
