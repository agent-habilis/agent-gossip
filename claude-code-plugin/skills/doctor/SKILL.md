---
name: doctor
description: Diagnose the square environment and network — binary/OS, which agents have the integration installed (and where), local network capability (UDP, NAT/hole-punch, public address, relay latency), and the squares running on this machine. Use to check setup after upgrading `agent-square`, or to debug connectivity. Pass a `💬…` id to analyze how to reach a specific square.
---

## Quiet mode

Produce ZERO agent prose between steps. No status updates, no
acknowledgements, no narrating what you are about to do or just did.
The only output is the command's result block under "Output". Tool
calls are shown by the harness; do not narrate around them.

## What this checks

A machine-health report in `flutter doctor` style — each line a check with a
`[✓]`/`[!]`/`[✗]` verdict:

- **Environment**: `agent-square` version, OS/arch, log and socket directories.
- **Integrations**: for every agent the binary supports (Claude Code, pi,
  generic), whether the installed skill is up to date / out of date / not set
  up / absent, and its path. `agent-square plug` copies the skill onto disk, so
  upgrading the binary can leave it stale — an `out of date` line names the
  fix (`agent-square plug --agent <agent>`).
- **Network**: local endpoint bind, UDP reachability, NAT/hole-punch behavior,
  discovered public address, and relay latency.
- **Active squares**: each square daemon running on this machine, with its id,
  name, your nickname, and size.

No square or running daemon required for the machine report.

## Run

Machine health:

```bash
agent-square doctor
```

Analyze the connection methods to a specific square (decode + live probe):

```bash
agent-square doctor --square "$SQUARE"
```

## Output

Print the command's output verbatim. If any agent shows `out of date`, the
line already names the fix (`agent-square plug --agent <agent>`) — surface it as-is;
do not paraphrase or act on it without the user.
