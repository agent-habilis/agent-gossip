/**
 * Turning a URL into a chain of routes.
 *
 *     const branches = flattenRoutes([
 *       { path: '/7guis', children: [{ path: ':task' }] },
 *     ])
 *     matchRoutes(branches, '/7guis/cells')
 *     // → [{ route: …, params: {}, pathname: '/7guis' },
 *     //    { route: …, params: { task: 'cells' }, pathname: '/7guis/cells' }]
 *
 * The tree is flattened into branches once, up front, and sorted by how
 * specific each one is; matching is then a linear scan that stops at the first
 * hit. The alternative — descending the tree and matching level by level — has
 * to decide what to do when a level offers two candidates, and the honest
 * answer is "whichever is more specific", which means computing the same score
 * anyway, only repeatedly and at match time. React Router arrived at the same
 * arrangement, and the scoring here is deliberately close to theirs.
 *
 * Like `history.ts`, this file imports nothing. It is generic over the route
 * payload rather than knowing about `ComponentFn`, so nothing here depends on
 * the view layer, or on there being a DOM.
 */

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

type Segment =
  | { readonly kind: 'static'; readonly value: string; readonly depth: number }
  | {
      readonly kind: 'param'
      readonly name: string
      readonly optional: boolean
      readonly depth: number
    }
  | { readonly kind: 'wildcard'; readonly name: string; readonly depth: number }

/** The shape `matchRoutes` needs. Real routes carry more; this is all it reads. */
export interface MatchableRoute {
  readonly path: string
  readonly children?: readonly MatchableRoute[]
}

export interface Branch<R> {
  /** Root to leaf. `matchRoutes` returns one `Matched` per entry. */
  readonly chain: readonly R[]
  /** The joined pattern, for diagnostics and for `buildPath`. */
  readonly pattern: string
  /** Higher is more specific. Branches arrive already sorted by it. */
  readonly score: number
  readonly segments: readonly Segment[]
  /** How many segments the chain has consumed by the end of each depth. */
  readonly depthEnds: readonly number[]
}

export interface Matched<R> {
  readonly route: R
  /** Every param matched by the whole branch, not just by this depth. */
  readonly params: Readonly<Record<string, string>>
  /** The portion of the pathname matched through this depth. */
  readonly pathname: string
}

// ---------------------------------------------------------------------------
// Compiling
// ---------------------------------------------------------------------------

function trimSlashes(path: string): string {
  let from = 0
  let to = path.length
  while (from < to && path[from] === '/') from++
  while (to > from && path[to - 1] === '/') to--
  return path.slice(from, to)
}

/** Split a pathname into its non-empty parts, so trailing slashes stop mattering. */
function toParts(pathname: string): string[] {
  const trimmed = trimSlashes(pathname)
  return trimmed === '' ? [] : trimmed.split('/')
}

export function joinPaths(parent: string, child: string): string {
  const head = trimSlashes(parent)
  const tail = trimSlashes(child)
  if (tail === '') return `/${head}`
  if (head === '') return `/${tail}`
  return `/${head}/${tail}`
}

function compileSegment(raw: string, depth: number, pattern: string): Segment {
  if (raw.startsWith(':')) {
    const optional = raw.endsWith('?')
    const name = raw.slice(1, optional ? -1 : undefined)
    if (name === '') {
      throw new Error(
        `visage-router: "${pattern}" has a ":" with no name after it. ` +
          'Write `:id` for a parameter, or escape the colon if it is meant to be literal.',
      )
    }
    return { kind: 'param', name, optional, depth }
  }
  if (raw.startsWith('*')) {
    return { kind: 'wildcard', name: raw.slice(1) === '' ? '*' : raw.slice(1), depth }
  }
  return { kind: 'static', value: raw, depth }
}

function compile(pattern: string, depths: readonly number[]): Segment[] {
  const raw = toParts(pattern)
  const segments = raw.map((part, i) => compileSegment(part, depths[i] ?? 0, pattern))
  for (let i = 0; i < segments.length - 1; i++) {
    if (segments[i]?.kind === 'wildcard') {
      throw new Error(
        `visage-router: "${pattern}" has a wildcard that is not the last segment. ` +
          'A wildcard consumes everything left, so nothing can follow it.',
      )
    }
  }
  return segments
}

/**
 * How specific a pattern is. A static segment is worth much more than a
 * parameter, so `/users/new` beats `/users/:id` however they were declared, and
 * a longer pattern beats a shorter one only when it is not paying for the
 * length in parameters.
 */
function score(segments: readonly Segment[], isIndex: boolean): number {
  // An index child contributes no segments of its own, so it would otherwise
  // tie with its parent's bare branch and lose on declaration order. It should
  // win: it is the more specific way to say "exactly this path".
  let total = segments.length + (isIndex ? 2 : 0)
  for (const segment of segments) {
    if (segment.kind === 'static') total += 10
    else if (segment.kind === 'wildcard') total -= 2
    else total += segment.optional ? 2 : 3
  }
  return total
}

/**
 * Flatten a route tree into branches, most specific first.
 *
 * Every node becomes a branch, not only the leaves, so a parent route can match
 * on its own — `/7guis` renders the layout with an empty outlet, and
 * `/7guis/cells` renders the layout with the spreadsheet inside it.
 */
export function flattenRoutes<R extends MatchableRoute>(routes: readonly R[]): Branch<R>[] {
  const branches: Branch<R>[] = []

  const walk = (route: R, parents: readonly R[], parentPattern: string): void => {
    const chain = [...parents, route]
    const pattern = joinPaths(parentPattern, route.path)

    // Which depth each segment of the joined pattern belongs to. Built by
    // counting parts as the chain is descended, so a route contributing no
    // segments (an index child, or a pathless layout) simply adds none.
    const depths: number[] = []
    let joined = ''
    for (let depth = 0; depth < chain.length; depth++) {
      const before = toParts(joined).length
      joined = joinPaths(joined, chain[depth]?.path ?? '')
      const after = toParts(joined).length
      for (let i = before; i < after; i++) depths.push(depth)
    }

    const segments = compile(pattern, depths)
    const depthEnds = chain.map((_, depth) => {
      let count = 0
      for (const segment of segments) if (segment.depth <= depth) count++
      return count
    })

    const isIndex = chain.length > 1 && trimSlashes(route.path) === ''
    branches.push({ chain, pattern, score: score(segments, isIndex), segments, depthEnds })

    for (const child of route.children ?? []) walk(child as R, chain, pattern)
  }

  for (const route of routes) walk(route, [], '')

  // Stable, so two branches of equal specificity keep their declaration order.
  return branches.sort((a, b) => b.score - a.score)
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

function decode(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    // A lone `%` is not worth failing a navigation over.
    return value
  }
}

/**
 * Walk pattern and path together, recursively so that an optional segment can
 * be given up and retried. `stops[i]` records how much of the path had been
 * consumed after segment `i`, which is what splits the match back apart by
 * depth afterwards.
 */
function walkSegments(
  segments: readonly Segment[],
  si: number,
  parts: readonly string[],
  pi: number,
  params: Record<string, string>,
  stops: number[],
): boolean {
  if (si === segments.length) return pi === parts.length

  const segment = segments[si]
  if (segment === undefined) return false

  if (segment.kind === 'wildcard') {
    params[segment.name] = parts.slice(pi).map(decode).join('/')
    stops[si] = parts.length
    return true
  }

  if (segment.kind === 'static') {
    if (parts[pi] !== segment.value) return false
    stops[si] = pi + 1
    return walkSegments(segments, si + 1, parts, pi + 1, params, stops)
  }

  const part = parts[pi]
  if (part !== undefined) {
    params[segment.name] = decode(part)
    stops[si] = pi + 1
    if (walkSegments(segments, si + 1, parts, pi + 1, params, stops)) return true
    delete params[segment.name]
  }

  // Nothing here, or taking it did not work out further along.
  if (!segment.optional) return false
  stops[si] = pi
  return walkSegments(segments, si + 1, parts, pi, params, stops)
}

/** Match one branch. Exposed mainly so a caller can test a single pattern. */
export function matchBranch<R>(branch: Branch<R>, pathname: string): Matched<R>[] | null {
  const parts = toParts(pathname)
  const params: Record<string, string> = {}
  const stops: number[] = new Array<number>(branch.segments.length).fill(0)

  if (!walkSegments(branch.segments, 0, parts, 0, params, stops)) return null

  const frozen = Object.freeze(params)
  return branch.chain.map((route, depth) => {
    const end = branch.depthEnds[depth] ?? 0
    const consumed = end === 0 ? 0 : (stops[end - 1] ?? 0)
    return {
      route,
      params: frozen,
      pathname: consumed === 0 ? '/' : `/${parts.slice(0, consumed).join('/')}`,
    }
  })
}

/** First branch that matches, or `null`. Branches must come from `flattenRoutes`. */
export function matchRoutes<R>(
  branches: readonly Branch<R>[],
  pathname: string,
): Matched<R>[] | null {
  for (const branch of branches) {
    const matched = matchBranch(branch, pathname)
    if (matched !== null) return matched
  }
  return null
}

// ---------------------------------------------------------------------------
// Building paths back up
// ---------------------------------------------------------------------------

/**
 * The inverse of matching: fill a pattern in from a set of params, so a link
 * can be written as `buildPath('/users/:id', { id: '2' })` rather than spliced
 * together by hand.
 */
export function buildPath(
  pattern: string,
  params: Readonly<Record<string, string | undefined>> = {},
): string {
  const out: string[] = []
  for (const raw of toParts(pattern)) {
    if (raw.startsWith('*')) {
      const value = params[raw.slice(1) === '' ? '*' : raw.slice(1)]
      if (value !== undefined && value !== '') out.push(value)
      continue
    }
    if (!raw.startsWith(':')) {
      out.push(raw)
      continue
    }
    const optional = raw.endsWith('?')
    const name = raw.slice(1, optional ? -1 : undefined)
    const value = params[name]
    if (value === undefined || value === '') {
      if (optional) continue
      throw new Error(
        `visage-router: "${pattern}" needs a "${name}" param and none was given.`,
      )
    }
    out.push(encodeURIComponent(value))
  }
  return out.length === 0 ? '/' : `/${out.join('/')}`
}

// ---------------------------------------------------------------------------
// Path arithmetic
// ---------------------------------------------------------------------------

export interface PathParts {
  readonly pathname: string
  readonly search: string
  readonly hash: string
}

/** Split `/a/b?x=1#top` into its three pieces, each including its punctuation. */
export function parsePath(path: string): PathParts {
  let rest = path
  let hash = ''
  const hashAt = rest.indexOf('#')
  if (hashAt !== -1) {
    hash = rest.slice(hashAt)
    rest = rest.slice(0, hashAt)
  }
  let search = ''
  const searchAt = rest.indexOf('?')
  if (searchAt !== -1) {
    search = rest.slice(searchAt)
    rest = rest.slice(0, searchAt)
  }
  return { pathname: rest === '' ? '/' : rest, search, hash }
}

/**
 * Resolve `to` against the page it was written on, so a link can say `../edit`
 * and mean it. Absolute targets pass through; `.` and `..` are folded the way a
 * browser folds them.
 */
export function resolvePath(to: string, from: string): string {
  const target = parsePath(to)
  const suffix = `${target.search}${target.hash}`

  if (to === '' || to.startsWith('?') || to.startsWith('#')) {
    // Same page, different query or fragment.
    return `${parsePath(from).pathname}${suffix}`
  }

  const parts = target.pathname.startsWith('/')
    ? []
    : // Relative links resolve against the directory, so `b` next to `/a/b`
      // means `/a/b` rather than `/a/b/b`.
      toParts(parsePath(from).pathname).slice(0, -1)

  for (const part of toParts(target.pathname)) {
    if (part === '.') continue
    if (part === '..') parts.pop()
    else parts.push(part)
  }

  return `${parts.length === 0 ? '/' : `/${parts.join('/')}`}${suffix}`
}
