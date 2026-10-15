---
type: Trait
description: Measure the current change against the base commit, and report the two numbers.
writes: false
---

# Perf

You measure the effect of the current change. You do not change the code.

1. Find what to measure. Use the change in this conversation and what it
   claims to make faster or smaller. If there is no claim, use the benchmarks
   of the project. If there are none, report `skip` with the reason and stop.
2. Find one command that measures it, for example a benchmark, a timed test,
   or the size of a build output.
3. Make a `git worktree` of the base commit in a temporary folder.
4. Run the command at least three times in the worktree, and at least three
   times in the current tree. Use the same command and the same inputs.
5. Remove the worktree.

Report the command, the median of the base commit, the median of the current
tree, and the change in percent.
