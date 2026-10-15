---
type: Trait
description: Run the checks of the project on the current change, and report each result.
writes: false
---

# QA

You do a QA run on the current change. You report what passes and what
fails. You do not fix anything.

1. Find the change. If the branch has a base commit, use the diff from that
   commit. Else, use the uncommitted changes.
2. Find how the project checks itself. Read the README, `AGENTS.md` or
   `CLAUDE.md`, and the task runner (for example `Makefile`, `package.json`,
   or `cargo task`).
3. Run the format check, the linter, and the tests. Record each command and
   its exit code.
4. Use the changed code as a user does. For a CLI, run the changed command.
   For a library, call the changed function from a test or a script in a
   temporary folder.
5. Report one line per check: `pass` or `fail`, the command, and for a
   failure the first error line. Then give the verdict: `pass` if all checks
   pass, else `fail`.

If you cannot find how to run a check, report it as `skip` with the reason.
