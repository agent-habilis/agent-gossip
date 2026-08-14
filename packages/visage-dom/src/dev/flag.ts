/**
 * Development build flag, keyed off `NODE_ENV`.
 *
 * `true` unless something says otherwise, so the guards fail safe: unbundled in
 * a browser there is no `process`, the optional chain yields `undefined`, and
 * every check stays on. Any bundler running a production build substitutes
 * `process.env.NODE_ENV` with `"production"`, this folds to `false`, and every
 * `if (DEV)` branch and its message strings fall to dead-code elimination —
 * including for a consumer compiling `src/` themselves, who gets that for free
 * and without configuring anything.
 *
 * Two forms that look equivalent and are not, both measured:
 *
 * - `typeof process === 'undefined' || process.env.NODE_ENV !== 'production'`
 *   is the careful-looking one and the worst of the options. The `typeof` test
 *   is opaque to the bundler, so `DEV` never becomes a constant, nothing folds,
 *   and a `process` reference survives into a browser bundle.
 * - `process.env.NODE_ENV !== 'production'`, without `globalThis`, folds fine
 *   but throws a `ReferenceError` when this source is loaded as plain ESM in a
 *   browser — the no-build-step path the library advertises.
 *
 * Our own bundle build (scripts/build.ts) does not rely on any of this: Bun
 * propagates no constants across module boundaries, so it blanks the import of
 * `DEV` and defines it directly. See the comment on `foldDev` there.
 *
 * Not re-exported from `src/index.ts`: this is not public API.
 *
 * The cast keeps this compiling in programs without the Bun/Node globals —
 * the app's tsconfig deliberately sets `types: []`, and tsc typechecks this
 * vendored source as part of the app program.
 */
export const DEV =
  (globalThis as typeof globalThis & { process?: { env?: { NODE_ENV?: string } } }).process?.env
    ?.NODE_ENV !== 'production'
