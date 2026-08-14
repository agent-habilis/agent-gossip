/**
 * Typed context tokens.
 *
 * A token is an identity, not a store. It carries a symbol the reconciler keys
 * on, a name for error messages, and an optional default; the values themselves
 * live on the component instances that provided them, and `this.inject` finds
 * the nearest one by walking up the component tree.
 *
 * There is no provider component. `component()` builds descriptors with an
 * empty children array, so a component cannot take positional children, and a
 * `<Theme.Provider>` wrapper would read well in JSX and badly everywhere else.
 * Providing from `this` works the same from both front ends.
 */

/** The `defaultValue` slot when a token was created without one. */
const NO_DEFAULT: unique symbol = Symbol('view.context.no-default')

export interface Context<T> {
  /** What instances key on. Unique per token, so two tokens never collide. */
  readonly id: symbol
  /** Used in the error thrown when nothing provided a required token. */
  readonly name: string
  readonly defaultValue: T | typeof NO_DEFAULT
  /** Phantom field: without it `Context<string>` and `Context<number>` unify. */
  readonly __type?: T
}

/**
 * Create a context token.
 *
 * Called without a default, `inject` throws when no ancestor provided a value.
 * That is usually what you want — a missing router or store is a bug, not a
 * case to handle.
 *
 *     const Theme = context<string>('theme', 'light')
 *
 * The value is whatever you provide, and nothing re-renders when it changes.
 * Provide a `Signal` when you want updates to propagate: reading `.value`
 * inside a generator subscribes through the ordinary tracking window, so
 * context needs no propagation rules of its own.
 */
export function context<T>(name: string, ...defaultValue: [T] | []): Context<T> {
  return {
    id: Symbol(name),
    name,
    defaultValue: defaultValue.length > 0 ? (defaultValue[0] as T) : NO_DEFAULT,
  }
}

export function hasDefault<T>(ctx: Context<T>): boolean {
  return ctx.defaultValue !== NO_DEFAULT
}

export function defaultOf<T>(ctx: Context<T>): T {
  return ctx.defaultValue as T
}
