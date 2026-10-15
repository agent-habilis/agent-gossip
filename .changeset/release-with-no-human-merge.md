---
default: patch
---

# Release with no human merge

The merge of a PR with a change file now runs the full release. The version bump goes straight to `main`, and the Homebrew formula goes straight to `agent-habilis/homebrew-tap`. No release PR and no formula PR wait for an admin merge. This release is the first one that the new flow cuts.
