# skills/ — sources, not what ships

These are the *sources* for the Agent Skills the binary embeds. `build.rs`
renders them into a `SKILL.md` per skill, plus the `references/*.md` files that
the template names (in `$OUT_DIR/skills`), and that generated tree is what
`include_dir!` embeds and `agent-gossip plug` installs. agy loads only the first
24000 bytes of a skill body, so a test fails any `SKILL.md` body that reaches
24000 bytes. The `SKILL.md` holds what the agent needs before its first tool
call. The long rules move to a reference file, and the body says when to read
it.

Rules:

- **`SKILL.md` is the template, and a `reference` line makes the other emitted
  files.** It declares its own sources with include directives, so adding a
  skill needs no `build.rs` change. Every other `.md` is a partial it (or
  another partial) pulls in. One own-line
  `<!-- reference name="events" path="../shared/events.md" -->` renders the
  file at `path` to `references/events.md` beside the `SKILL.md`, and the
  directive line never reaches the rendered `SKILL.md`. The body must name
  each reference and say when to read it: a test fails a reference that no
  body names, and a name that no file answers.
- Directives are HTML comments — template sources stay valid, cleanly
  rendering markdown (include lines vanish in preview). One coherent grammar,
  all inputs named and double-quoted, `\"` and `\\` the only escapes:

  ```markdown
  <!-- include path="../shared/daemon-session.md" launch="agent-gossip join \"{TARGET}\"" noun="line" -->
  <!-- slot name="launch" -->
  ```

  `include` (alone on its own line) splices the file at `path`, resolved
  relative to the file containing the line, rendered with exactly the other
  keys as its args. `slot` (anywhere) is the substitution point such an arg
  fills.
- Rendering is the `slot-template` workspace crate: a pure function — an
  included file sees only the args at its call site, nothing inherited.
  Strict both ways: a slot with no arg, an arg with no slot, and any
  malformed directive fail the build. Ordinary HTML comments pass through.
- Partials use `##`/`###` headings — in the rendered file they are sections
  under the skill's `# name` heading. Cross-references are section names
  ("per the **Event handling** section"), never file paths. The one exception
  is the pointer to a reference file, which names its `references/` path.

This file is in the embed SKIP list (`src/cli/embed_skip.rs`) — it documents
the sources and never ships.
