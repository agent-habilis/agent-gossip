---
default: major
---

# Cut idle cost and wire bytes; frame version is now 13.0

The daemon does less work when the mesh is calm and sends fewer bytes. The meta document resets to a new epoch when dead peers fill it, so memory stays bounded. Digest window ids are base64. A peer on frame version 12.0 cannot read frames of version 13.0: all peers of a mesh must update together.
