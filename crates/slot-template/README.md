# slot-template

Pure-function markdown templating over exactly two HTML-comment directives, so
a template source stays valid, cleanly-rendering markdown. It has one job:
turn the multi-file [`skills/`](../../skills) sources into the single-file
`SKILL.md` tree the `agent-gossip` binary embeds.

No engine, no ambient context, no escaping of substituted values, no
dependencies at all — one `expand()` function and one `Error` enum.

## Why it exists

An agent invoking a skill must get the whole procedure in the file it was
already handed: zero extra read round-trips. That makes one self-contained
`SKILL.md` per skill the runtime contract.

But the fifteen skills share a great deal — the reattach procedure, the
receive-loop contract, the bell guard, the event catalogue. Keeping fifteen
copies in sync by hand is how the bell-guard rules drifted between skills
before. So the sources stay multi-file (`skills/<name>/SKILL.md` +
`workflow.md`, splicing `skills/shared/*.md` partials) and
[`agent-gossip`'s `build.rs`](../agent-gossip/build.rs) renders them at build
time. Adding a skill needs no change to the build script.

The directives are HTML comments for a reason: a skill source is read and
edited far more often than it is rendered, so it has to stay valid markdown
that previews correctly in any viewer. A `{{ mustache }}` syntax would not.

## The two directives

**`<!-- include path="..." key="value" ... -->`** — must be alone on its own
line. Splices another file, resolved relative to the file containing the line,
rendered with **exactly** the args named at that call site. Spliced content is
final: the includer never re-substitutes it.

**`<!-- slot name="..." -->`** — anywhere, inline. The substitution point an
include arg fills.

```markdown
<!-- skills/gossip-msg/SKILL.md -->
<!-- include path="../shared/quiet.md" -->
<!-- include path="workflow.md" -->
<!-- include path="../shared/receive-loop.md" -->
```

```markdown
<!-- skills/shared/guard.md -->
If <!-- slot name="required_session_vars" --> is missing, follow the
**Reattach** section and …
```

```markdown
<!-- the call site that fills it -->
<!-- include path="../shared/guard.md" required_session_vars="`$GOSSIP` or `$NICKNAME`" -->
```

### Strict in both directions

A slot with no arg is an error, **and** an arg no slot consumes is an error.
That is the whole design: a call site is checked like a function signature, so a
renamed slot or a dropped arg fails the build instead of silently rendering a
gap or quietly ignoring a value.

A comment that starts as a directive but fails the grammar is an error too,
never silently literal. Every other HTML comment and every other byte passes
through byte-exact.

## The API

```rust
pub fn expand(
    path: &Path,
    args: &[(&str, &str)],
    loader: &mut dyn FnMut(&Path) -> Result<String, String>,
) -> Result<String, Error>
```

That is the entire public surface, plus `Error`: `UnknownSlot`, `UnusedArg`,
`MalformedSlot`, `MalformedInclude`, `IncludeCycle`, `DepthExceeded` (include
graphs nest at most 8 deep), and `Load` (the caller's loader failed).

I/O is the caller's: `expand` reads nothing itself, it calls `loader`. That is
what makes it a pure function and what makes its 22 unit tests need no fixture
files on disk.

## Test it

```sh
cargo test -p slot-template
```

Everything lives in `src/lib.rs` — the tests included.

## What it is not

Not published (`publish = false`), not a general-purpose templating engine, and
not a markdown parser: it does not know what a heading or a code fence is, it
only knows the two directives and copies the rest. It has no loops, no
conditionals, and no expressions, and it should stay that way — the moment a
skill source needs logic, the logic belongs in the skill's procedure, not in the
renderer.
