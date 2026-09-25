---
default: minor
---

# Keep a task alive while both daemons run

Both daemons now beat every live task from the offer on, whoever holds the
ball. A party cancels a task only after 2 minutes with no leg or beat from the
other daemon. A pending accept question, a slow review, or a long build no
longer cancels the task at about 4 minutes, and skills no longer re-emit
`working` to keep a task alive. A task that neither agent touches for 24 hours
stops being beaten, and the other daemon cancels it about 2 minutes later, so
a task forgotten after a `/clear` still ends. A peer
on an older version is not cancelled for silence until it has sent a beat.
