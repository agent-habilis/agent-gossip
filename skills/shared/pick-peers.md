## Pick peers

Read the roster and metadata:

```bash
agent-gossip peers --gossip "$GOSSIP" --nickname "$NICKNAME"
agent-gossip meta get --gossip "$GOSSIP" --nickname "$NICKNAME"
```

The candidates are exactly the roster's `peers` — never yourself, and
never a nickname that appears only in the meta document. The meta document is
not pruned when a peer leaves and includes your own entry; it only decorates
candidates with model, harness, host, and status. An empty `peers`
array is the empty-roster stop — do not use the response's `count`, which
includes self.

No candidate is excluded — quiet and `busy` peers stay candidates; their
state is shown, never filtered. Rank all candidates by meta status (`idle`,
then `available`, then unreported, then `busy`) and, within the same status,
by most recently seen (smallest roster last-seen; a peer with no last-seen
yet has just joined — rank it most recent, not least); quiet peers always
sort last, regardless of status. This order is the **availability ranking** —
it orders the options in the next section, nothing more.

Only an empty roster stops the flow. Print:

```text
💬️ no peers in the gossip
```

Then stop.

## Choose peers

Put the choice to the user per the **Decisions** section. The experience is
the same at every roster size — one peer gets the same multi-select as
twenty; there is no single-peer special case.

A nickname is always written `<nick>`, the angle brackets literal characters
kept in the rendered text — in option labels, in question-text lists, in skip
lines — never bare. A peer mentioned in question text also carries its model
from the meta document when known — `<nick> (model)`, e.g.
`<throne-orbit> (Opus 4.8)` — but not in an option label, where the
description already shows the model. Wherever a peer is described, include
its `host` only when it differs from this machine's `hostname -s`. Omit any
unreported field.

Ask ONE multi-select question — "Which peers?":

- The last option is always `cancel — send to no one`. It guarantees the
  widget's minimum option count when only one peer exists, and gives an
  explicit way out at every size (an empty submission is not guaranteed on
  every widget). If `cancel` is selected — alone or alongside peers — the
  flow stops with `💬️ no peers selected`.
- The peer options fill the remaining slots: the top peers by the
  availability ranking, up to one less than the widget's option cap (top 3 on
  Claude Code's `AskUserQuestion`, which allows 4 options and always appends
  its own "Other" free-text entry; top 4 on a 5-option widget). Label
  `<nick>`, description `model · harness · status` from the meta document,
  with `host` inserted before `status` for a peer on another machine and
  `· quiet (may be gone)` appended for a quiet peer — a peer's idleness is
  visible in every option.
- Manual entry is always available: where the widget has native free text
  (Claude Code's "Other") that is the path; otherwise the last option is
  `type nicknames…`, which prompts for them. Either way the input is
  comma-separated nicknames. When the roster exceeds the listed options, name
  the remaining peers (with status) in the question text so the user knows
  what to type.

The selected peers are exactly the selected options plus any typed nicknames.
Skip a typed nickname that is not in the roster, printing one line per skip:

```text
💬️ <nick> not in the gossip · skipped
```

If nothing remains, print `💬️ no peers selected` and stop.

Hold the result as the **selected peers**; the next section fans out to them.
