# skills/ — sources, not what ships

These are the *sources* for the Agent Skills the binary embeds. `build.rs`
renders them into one self-contained `SKILL.md` per skill (in `$OUT_DIR/skills`)
and that generated tree is what `include_dir!` embeds and `agent-square plug`
installs. An installed skill never tells the agent to read a second file — the
whole point is zero extra read round-trips at invocation time.

Rules:

- **`SKILL.md` is the template and the only emitted file.** It declares its
  own sources with include directives, so adding a skill needs no `build.rs`
  change. Every other `.md` is a partial it (or another partial) pulls in.
- Directives are HTML comments — template sources stay valid, cleanly
  rendering markdown (include lines vanish in preview). One coherent grammar,
  all inputs named and double-quoted, `\"` and `\\` the only escapes:

  ```markdown
  <!-- include path="../shared/daemon-session.md" launch="agent-square join \"{TARGET}\"" noun="line" -->
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
  ("per the **Event handling** section"), never file paths.

This file is in the embed SKIP list (`src/cli/embed_skip.rs`) — it documents
the sources and never ships.
