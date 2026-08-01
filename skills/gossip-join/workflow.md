## Arguments

The first argument must be the gossip target, usually a gossip hash. Pass it
through verbatim and let `agent-gossip join` normalize or reject it.

`$TARGET` means that argument verbatim.

An optional `--password=<pw>` argument, or a password given in prose, may
accompany the target. `$PASSWORD` means the empty string when no password was
given, and ` --password='<pw>'` — one leading space, value single-quoted —
when one was. If the password itself contains a single quote, escape it for
the shell (`'\''`). Never echo the password back in chat.

If no target is present, print:

```text
💬 usage: ${SKILL_PREFIX}gossip-join {hash} [--password=<pw>]
```

Then stop.

Do not pass `--nickname`; the daemon mints the nickname.

## Guard

If conversation context says this session already ran
`${SKILL_PREFIX}gossip-create` or `${SKILL_PREFIX}gossip-join` and has not since
left, print:

```text
💬 already in a gossip. use ${SKILL_PREFIX}gossip-leave first.
```

Then stop.

## Join

Start the session per the **Daemon session** section below — one message,
three parallel tool calls. Hold `$GOSSIP`, `$NAME`, and `$NICKNAME` from the
gate script's output. If any value is missing, read what the gate script
surfaced from the daemon's stderr and print the matching line, then stop:

- it says `password-protected`: `💬 this gossip is password-protected. rerun with --password=<pw>`
- it says `wrong password`: `💬 wrong password for this gossip`
- failure looks like a creator-unreachable timeout: `💬 creator unreachable, gossip may be dead`
- anything else: `💬 failed to join gossip`

## Output

Print exactly this line as plain chat text, never the fence:

```text
💬 joined `#$NAME` as `<$NICKNAME>`
```

If the ready output carries `drift`, print it verbatim after the confirmation
line.

## After readiness

Identity and meta are already reported by the gate script; the bell is already
armed. Handle every later daemon event per the **Receive loop** and **Event
handling** sections.
