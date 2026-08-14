/**
 * The seam between reconciliation and whatever it is reconciling *into*.
 *
 * The reconciler was already host-agnostic in everything but its types: the
 * keyed diff, the LIS, the generator driver, live props, error escalation and
 * disposal never cared what a node was. Only fifteen call sites did. This
 * interface is those fifteen calls, and nothing else.
 *
 * Three implementations ship: `domHost` in `../dom/index.ts`, `canvasHost` in
 * `visage-canvas/src/scene/index.ts`, and `createTuiHost` in `visage-tui/src/node/index.ts`.
 *
 * Its own module, not a file under `../reconcile/`, because a host implementation
 * should not have to name the reconciler to describe itself. It has no imports
 * and no runtime — it bundles to nothing — so importing it costs a host exactly
 * zero, and `scripts/boundaries.test.ts` asserts that it stays that way.
 */

/**
 * A node in whatever tree the host maintains.
 *
 * Deliberately `object` rather than a branded type or a generic parameter.
 * `Element`, `Text`, `Comment` and a canvas `SceneNode` are all assignable to
 * it, so nothing has to be cast at the boundary — and, the actual point,
 * *nothing inside the reconciler can call a DOM method on one*. The compiler is
 * what proves the coupling is gone. The cost is losing DOM autocompletion inside
 * `index.ts`, which is a fair trade for a guarantee generics would not give.
 */
export type HostNode = object

/** A node that can hold children. Same type; the name documents the position. */
export type HostEl = HostNode

type Props = Readonly<Record<string, unknown>>

export interface Host {
  // --- Creation ------------------------------------------------------------

  createNode(tag: string): HostEl
  createText(value: string): HostNode
  /** Placeholder for a null/false child, so siblings keep stable positions. */
  createHole(): HostNode

  // --- Mutation ------------------------------------------------------------

  setText(node: HostNode, value: string): void
  applyProps(node: HostEl, next: Props, prev?: Props): void
  /**
   * Run a `ref`, returning whatever detach callback it handed back.
   *
   * A `ref` that stores the node somewhere the component does not own — a
   * parent's field, a module-level Map — is the classic detached-node leak, and
   * `using` cannot reach it because the reconciler calls the ref, not the
   * component. Returning a cleanup is how that gets undone, as in React.
   */
  runRef(props: Props, node: HostEl): (() => void) | void

  /** Insert before `before`, or append when it is null. */
  insert(parent: HostEl, node: HostNode, before: HostNode | null): void
  /** Detach from wherever the node currently is. Self-locating, like the DOM's. */
  remove(node: HostNode): void
  append(parent: HostEl, node: HostNode): void

  // --- Reading -------------------------------------------------------------

  nextSibling(node: HostNode): HostNode | null
  childCount(parent: HostEl): number
  /** Drop every child in one operation. Guarded by a `childCount` check. */
  clear(parent: HostEl): void

  // --- Optional fast paths and capabilities --------------------------------

  /**
   * A detached container for staging a run of siblings before one bulk insert.
   *
   * Returning `null` means "no fast path here" and `mountRange` inserts each
   * sibling directly. That is the right answer for a host where insertion is
   * already a pointer write — the staging would be pure overhead.
   */
  createFragment(): HostEl | null

  /**
   * Whether `portal()` means anything here. A host with no stacking contexts to
   * escape and no foreign containers to target says no, and `mount` throws
   * rather than half-working.
   */
  readonly supportsPortals: boolean

  /**
   * Called once per settled reconcile, at the outermost frame. The DOM host has
   * nothing to do — its writes are the commit. A retained host that paints from
   * its tree uses this to mark itself dirty.
   */
  commit?(): void
}
