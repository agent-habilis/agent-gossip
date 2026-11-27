---
default: patch
---

# Limit the probe of a daemon socket to 2 seconds

`leave`, `session` and `doctor` ask each daemon on the host for its identity.
A daemon that accepted the connection and never answered made each of them
wait with no limit. The probe now skips a daemon that gives no answer in 2 s.
