---
default: patch
---

# Limit the probe of a daemon socket to 2 seconds

`leave`, `doctor`, and a `create`, `join` or `topic` with `--state-file` ask
each daemon on the host for its identity. A daemon that accepted the
connection and never answered made each of them wait with no limit. The probe
now skips a daemon that gives no answer in 2 s, and the probes run 32 at a
time.

`peers` now exits with 1 when the daemon refuses the request, as `state`,
`meta` and `topology` do.

`a2a discover` now lists a bridge that `a2a expose --advertise` advertises.
Before, it read the wrong part of the frame and listed nothing.

`a2a artifact --file-name` and `--file-mime` now need `--file`, as in
`a2a call`. Before, they were ignored without `--file`.
