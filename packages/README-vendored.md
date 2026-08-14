# Vendored packages

The runtime and design system the `/room/` app is built on.

| package | what it is |
|---|---|
| `visage-dom` | the runtime — generator components, signals, JSX. No dependencies. |
| `visage-router` | routing. Depends on `visage-dom`. |
| `visage-style` | scoped `@scope` styles + tokens. |
| `moonspace` | the monospace grid and glyph constants. |
| `moonspace-theme` | the superstylin palette as 16 semantic colour roles. |
| `moonspace-dom` | the component library — `Box`, `Text`, `Stack`, `Input`, `Button`, `Badge`. |

They are vendored rather than depended on because there is nothing to depend
on: none is published to npm, and the upstream visage checkout
(`~/Developer/visage-ui/visage`) has no git remote configured at all.
`moonspace` upstream is `git@github.com:visage-ui/moonspace.git`.

## Where these came from

Copied on 2026-08-13 from **`agent-share/web/vendor/`**, not from upstream.

That is deliberate. agent-share's copy carries local patches on top of upstream
(listed below), and one of them changes how components may be written — taking
the upstream source instead would silently break the idiom this app uses. Copied
with `node_modules`, `dist` and `.git` excluded.

agent-share took its own copy on 2026-08-06 from
`~/Developer/visage-ui/visage/packages/*` @ `82e7506` **plus a dirty working
tree** — the `this`-based context API existed only as uncommitted changes at
that point. So `82e7506` alone does not reproduce these files.

agent-share dropped `moonspace-theme`'s `stories/` directory (and its
`./stories/*` export); that omission carries over here.

## Local patches carried from agent-share

Re-apply these when re-vendoring. Numbering follows agent-share's own
`vendor/README.md` so the two lists can be diffed.

1. `visage-style/src/compile/index.ts` — `flex` kept unitless (`flex: 1`, not
   `flex: 1px`), plus test.
2. `visage-dom/src/dom/index.ts` — `UNITLESS` property set + `cssNumber()`, so
   numeric inline styles like `opacity`/`flex` do not get `px` appended.
3. `visage-dom/src/jsx-runtime/index.ts` — plain stateless view functions usable
   directly in JSX (`<MyFn/>` without a `component()` wrapper), including key
   hoisting.
4. `moonspace/src/theme/grid.ts` — row height forked to 22.5px (line-height
   1.5); upstream is 18px.
5. `moonspace-dom/.../ProgressBar` — `fluid` prop, plus test.
6. `moonspace-dom/.../MiddleTruncate` — migrated to the `this`-based context.
7. `moonspace-dom/.../Button` — boxed presentation, replacing upstream's
   `[ brackets ]` style.

Patch 3 is the load-bearing one: without it, every presentational helper in
`../app/components/` has to be wrapped in `component()`.

## Workspace wiring

- Both packages are consumed **as TypeScript source** through their `exports` —
  no build, no `dist`, no `.d.ts`. `web/package.json` lists them under
  `workspaces` so `bun install` creates the `node_modules` symlinks.
- Their tsconfigs extend `web/tsconfig.base.json` and add `types: ["bun"]`.
- Their `bunfig.toml` files preload `../../scripts/test-setup.ts`, because Bun
  does not walk up for a bunfig — without it, `cd vendor/visage-dom && bun test`
  runs with no DOM registered.
- **They have no `build` script, on purpose.** Upstream's per-package library
  build was never vendored, so a `build` script here would match `web/`'s own
  bundler script and run that instead.

`tsconfig.build.json` in each package is upstream's declaration-emit config. It
is dead weight — nothing runs it — but is kept so the diff against agent-share
stays empty and a re-vendor is a plain copy.
