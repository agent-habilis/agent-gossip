---
default: minor
---

# Attach a file to `a2a call` with `--file`

`a2a call --file PATH [--file-name NAME] [--file-mime MIME]` sends a file with a new task or a follow-up, as `a2a artifact --file` does. The worker's `message` event carries the file as a `Part.url` in `payload`, and `a2a fetch` gets it. A relative path resolves against the caller's directory. The file lives as long as its task; a call that times out keeps no task. This bumps fofoca to 186459c, where one file sent on several tasks stays until the last of them ends.
