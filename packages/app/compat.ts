/**
 * Safari — through 27 — ships no Explicit Resource Management: `Symbol.dispose`
 * and `Symbol.asyncDispose` are both `undefined`. Two halves of the codebase
 * disagree about a disposable's key when that is the case:
 *
 * - source writes the well-known symbol directly, as in
 *   `{ [Symbol.dispose]() {} }` (visage-dom's whole `resources` module), and a
 *   computed `undefined` key stringifies to the property `"undefined"`;
 * - the `usingCtx` helper the `using` downlevel emits looks the method up as
 *   `obj[Symbol.dispose || Symbol.for('Symbol.dispose')]`.
 *
 * Left alone the two never meet and every disposal throws `Object is not
 * disposable`. Defining the symbols as the registered ones the helper falls
 * back to makes both sides name the same property.
 *
 * This has to run before any module that builds a Disposable, so entry points
 * import it first. Where the symbols are native — every other browser, and Bun
 * for the test suites — both assignments are no-ops.
 */
const wellKnown = Symbol as { dispose?: symbol; asyncDispose?: symbol }

wellKnown.dispose ??= Symbol.for('Symbol.dispose')
wellKnown.asyncDispose ??= Symbol.for('Symbol.asyncDispose')

export {}
