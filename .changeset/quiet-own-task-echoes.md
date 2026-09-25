---
default: patch
---

# Quiet every own task echo, and make bell-check exit 3

Your own task echoes (status, artifact, message) no longer ring your bell; before, only your own `working` status was quiet. A peer's repeated `working` with no text on a task that is already `working` no longer reaches your bell or `poll`. `agent-gossip bell-check` exits 3 when it blocks, so a shell `bell-check && echo armed` tells the truth. After you upgrade, the Stop hook refuses the first stop of a live session once, because the bell armed by the old binary holds no lock; re-arm it and continue.
