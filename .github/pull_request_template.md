<!--
PR body template (agreed 2026-11-05). Rules:
- Declarative present tense. Every claim carries a test name, a commit hash or a measurement.
- Rewrite the whole body on every push. Do not append rounds.
- Review rounds do not live in the body. Save the old body verbatim in a pinned "Review log" comment
  (comment limit 65,536 chars: chunk at ~58,000 at "## " boundaries).
- Keep the tasklist markers: tools replace the text between them.
- Style: ASD-STE100 (short sentences, active voice, digits for numbers).
- Target size: 100 lines for a small PR, 150-250 lines for a large one.
-->

Closes #<issue>. <One line: the problem in the issue.>

## Summary

<5 lines at most. What changes, why, and what the reader must know before merge.>

## Merge checklist

<!-- tasklist:start -->
- [ ] Merge order: <PR A>, then <PR B>
- [ ] Pin equals the head of <PR A> in every file that pins it
- [ ] CI green on head `<sha>`
- [ ] Post-merge steps: <release, downstream pins, follow-up PRs>
- [ ] Human review and merge (no agent merges)
<!-- tasklist:end -->

## Breaking changes

| What | Before | After | Who is affected |
|---|---|---|---|
| <wire version, file format, API> | | | |

## What changed

<6-8 blocks. Each block: a bold name, 3-4 lines, commit hashes.>

**<Area>.** <What it does and why.> (`<sha>`)

## Design decisions

<4 entries at most. Each: the choice, the alternative, the reason.>

- **<Decision>.** Chosen: <x>. Rejected: <y>. Reason: <z>.

## Evidence

- Red then green: `<test_name>` failed on `<sha>` for <reason>, passes on `<head>`.
- Measured on `<head>`: <suite> <n> passed, <n> failed. Flakes: <name>, <rate>.
- CI: <state>.

## Known limits

<One ranked list. Each line: limit, impact, likelihood, follow-up.>

1. <limit>. Impact: <x>. Likelihood: <y>. Follow-up: <z>.

## Review history

See the pinned "Review log" comments.
