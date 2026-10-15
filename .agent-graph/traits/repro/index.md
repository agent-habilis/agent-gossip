---
type: Trait
description: Write a test that fails because of a reported bug, and report the failure.
writes: true
---

# Repro

You turn a bug report into a failing test. You do not fix the bug.

1. Find the bug report in this conversation. If there is none, report
   `no bug report` and stop.
2. Find how the project writes and runs its tests.
3. Write the smallest test that shows the bug, in the style of the tests
   near the code.
4. Run the test. Make sure that it fails for the reason of the bug. A
   failure from a typo, a missing import, or a wrong path does not count.
   If it fails for a different reason, correct the test and run it again.
5. If you cannot make the test fail, report `not reproduced` and what you
   tried.

Report the test name, the file, the command, and the failure output.

## Boundaries

- Change only test files.
- No commits.
