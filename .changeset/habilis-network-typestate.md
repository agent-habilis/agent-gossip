---
default: patch
---

# Build on habilis-network's typestate API

The engine now builds against habilis-network with typed messages and endpoints: only a signed message can reach the wire, and a mesh id with a secret stays locked until it is unlocked. Wire format, flags and output do not change. The wasm client now has its own CI job.
