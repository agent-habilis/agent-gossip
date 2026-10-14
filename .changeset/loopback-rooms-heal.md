---
default: patch
---

# Loopback rooms heal after a split

A loopback room no longer splits into meshes that never meet, and a holder sheds a beacon that stopped answering. A member that let go of the rendezvous comes back to it once in a while, and the hidden flag `--rendezvous-periodic-return-secs` sets the wait. When every port of a private room is held, a member prints one `Info` line, at most once in five minutes. This bumps habilis-network to c2526d6, the merge of PR #16 and PR #17.
