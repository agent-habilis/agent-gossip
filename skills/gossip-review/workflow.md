<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->

## Review subject

Use the argument text as the review subject. If no argument is present, use
the work at the center of the current conversation — the plan, diff, or
proposal being produced or discussed.

Hold a one-line label of the subject as `$SUBJECT_LABEL`; it names the review
in the report header and the todo subjects.

Condense the subject for review quality, not for size — the wire carries
large subjects fine. If the raw plan or diff is sprawling, reduce it to its
key claims, decisions, and load-bearing excerpts, and mark it `(condensed)`.
A subject past a few tens of kilobytes also stops healing via gossip
anti-entropy and rides a direct transfer instead — prefer condensing a long
diff over pasting it whole.

<!-- include path="../shared/pick-peers.md" -->

## Send

Send one task per selected peer — a directed `SendMessage` carrying no
`--task-id`, which is what makes it a new task:

```bash
agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$PEER" --method SendMessage --label "review: $SUBJECT_LABEL" --text "$BRIEF"
```

`--label` is what makes the reviewer's todo row read the same as yours.

`$BRIEF` is this template with `$NICKNAME` and `$SUBJECT` spliced in:

```text
ADVERSARIAL REVIEW · from <$NICKNAME>. Attack the subject below: try to refute it, find failure scenarios, edge cases, and concrete counterexamples. Do not summarize it, do not praise it, do not suggest polish: only defects that would make it fail.

SUBJECT:
$SUBJECT

Return ONE artifact on this task containing your findings as a numbered list. Each finding: severity (critical/major/minor), confidence (high/medium/low), a one-line claim, and a concrete failure scenario (specific inputs or state -> the wrong outcome). If nothing survives your attack, return exactly: no findings survived. After the artifact, wait for approval, then close the task.
```

Capture `result.task.id` as that reviewer's `$TASK_ID`. Track each task per
the **Task tracking** rules in the Event handling section, its label the same
`review: $SUBJECT_LABEL` you sent. Every reviewer gets the same brief, so the
rows differ only by counterparty and badge — that is honest, the tasks are
identical.

## Drive

Follow the task event rules in the **Event handling** section and the
**Receive loop** contract — one batch per turn, print last, act first.

On `input-required` kind `artifact-update` from a reviewer: approve with a
`--task-id` follow-up so the reviewer closes, then print the findings:

```bash
agent-gossip a2a call --gossip "$GOSSIP" --nickname "$NICKNAME" --to "$PEER" --method SendMessage --task-id "$TASK_ID" --text "findings received — close the task"
```

```text
💬 `<$PEER>` · review findings

$ARTIFACT_TEXT
```

Drop `--task-id` and you have not approved anything — you have opened a second
task on that reviewer.

On `input-required` kind `status-update` — a reviewer's question — answer it
when the answer is clear from the subject; otherwise put it to the user per
the **Decisions** section.

The reviewer authors the terminal `completed` once you approve; you never set
a task's state.

## Merge

Run this only when every review task has reached a terminal state
(`completed`, `failed`, `task_timeout`). Close the remaining todos first, then
print the report as the final output of the turn:

- **Dedupe:** findings from different reviewers naming the same failure
  mode or scenario are one entry — keep the highest severity and confidence
  claimed, attributed to every reviewer who raised it.
- **Rank:** severity (`critical`, `major`, `minor`), then confidence (`high`,
  `medium`, `low`), then how many reviewers raised it independently.

Separate the numbered findings with a blank line:

```text
💬 adversarial review · $SUBJECT_LABEL · $R reviewers · $M findings

1. **critical** · high · <claim> · <failure scenario> · `<nick-a>`, `<nick-b>`

2. **major** · medium · <claim> · <failure scenario> · `<nick-c>`

no findings survived: `<nick-d>`
dropped (failed/timed out): `<nick-e>`
```

Omit the `no findings survived:` and `dropped:` lines when empty. If every
reviewer returned "no findings survived", the body is the single line
`no findings survived — <nicks>`.
