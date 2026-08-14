import { test, expect } from 'bun:test'
import {
  buildPath,
  flattenRoutes,
  joinPaths,
  matchRoutes,
  parsePath,
  resolvePath,
} from './index.ts'

interface Route {
  readonly path: string
  readonly id?: string
  readonly children?: readonly Route[]
}

/** Match and report the ids of the chain, which is what the assertions care about. */
function hit(routes: readonly Route[], pathname: string): string[] | null {
  const matched = matchRoutes(flattenRoutes(routes), pathname)
  return matched === null ? null : matched.map((one) => one.route.id ?? one.route.path)
}

function paramsOf(
  routes: readonly Route[],
  pathname: string,
): Readonly<Record<string, string>> | null {
  const matched = matchRoutes(flattenRoutes(routes), pathname)
  return matched === null ? null : (matched[matched.length - 1]?.params ?? null)
}

// ---------------------------------------------------------------------------
// Flat matching
// ---------------------------------------------------------------------------

test('a static pattern matches exactly', () => {
  const routes: Route[] = [{ path: '/todo' }]
  expect(hit(routes, '/todo')).toEqual(['/todo'])
  expect(hit(routes, '/todos')).toBeNull()
  expect(hit(routes, '/todo/extra')).toBeNull()
})

test('trailing slashes are not a distinction', () => {
  const routes: Route[] = [{ path: '/todo' }]
  expect(hit(routes, '/todo/')).toEqual(['/todo'])
  expect(hit(routes, '/')).toBeNull()
  expect(hit([{ path: '/' }], '')).toEqual(['/'])
})

test('a parameter captures one segment', () => {
  const routes: Route[] = [{ path: '/users/:id' }]
  expect(paramsOf(routes, '/users/2')).toEqual({ id: '2' })
  // One segment, not several, and not none.
  expect(hit(routes, '/users')).toBeNull()
  expect(hit(routes, '/users/2/edit')).toBeNull()
})

test('parameter values are percent-decoded', () => {
  expect(paramsOf([{ path: '/tags/:name' }], '/tags/two%20words')).toEqual({
    name: 'two words',
  })
})

test('a malformed escape is passed through rather than thrown', () => {
  expect(paramsOf([{ path: '/tags/:name' }], '/tags/100%')).toEqual({ name: '100%' })
})

test('an optional parameter may be absent', () => {
  const routes: Route[] = [{ path: '/posts/:page?' }]
  expect(paramsOf(routes, '/posts/2')).toEqual({ page: '2' })
  expect(paramsOf(routes, '/posts')).toEqual({})
})

test('an optional parameter in the middle backs off when the rest will not fit', () => {
  // Taken greedily, `:lang?` would swallow "edit" and then find nothing left to
  // match "edit" against. It has to be given up and retried.
  const routes: Route[] = [{ path: '/docs/:lang?/edit' }]
  expect(paramsOf(routes, '/docs/edit')).toEqual({})
  expect(paramsOf(routes, '/docs/en/edit')).toEqual({ lang: 'en' })
  expect(hit(routes, '/docs/en/fr/edit')).toBeNull()
})

test('a wildcard captures everything left, including nothing', () => {
  const routes: Route[] = [{ path: '/files/*' }]
  expect(paramsOf(routes, '/files/a/b/c.txt')).toEqual({ '*': 'a/b/c.txt' })
  expect(paramsOf(routes, '/files')).toEqual({ '*': '' })
})

test('a wildcard can be named', () => {
  expect(paramsOf([{ path: '/files/*rest' }], '/files/a/b')).toEqual({ rest: 'a/b' })
})

// ---------------------------------------------------------------------------
// Specificity
// ---------------------------------------------------------------------------

test('a static segment beats a parameter regardless of declaration order', () => {
  const routes: Route[] = [
    { path: '/users/:id', id: 'param' },
    { path: '/users/new', id: 'static' },
  ]
  expect(hit(routes, '/users/new')).toEqual(['static'])
  expect(hit(routes, '/users/2')).toEqual(['param'])
})

test('a parameter beats an optional, and both beat a wildcard', () => {
  const routes: Route[] = [
    { path: '/a/*', id: 'wildcard' },
    { path: '/a/:x?', id: 'optional' },
    { path: '/a/:x', id: 'param' },
  ]
  expect(hit(routes, '/a/1')).toEqual(['param'])
})

test('a wildcard is the last resort, so it works as a catch-all', () => {
  const routes: Route[] = [
    { path: '/*', id: 'notFound' },
    { path: '/todo', id: 'todo' },
  ]
  expect(hit(routes, '/todo')).toEqual(['todo'])
  expect(hit(routes, '/anything/at/all')).toEqual(['notFound'])
})

test('equally specific patterns keep their declaration order', () => {
  const routes: Route[] = [
    { path: '/a/:x', id: 'first' },
    { path: '/a/:y', id: 'second' },
  ]
  expect(hit(routes, '/a/1')).toEqual(['first'])
})

// ---------------------------------------------------------------------------
// Nesting
// ---------------------------------------------------------------------------

const NESTED: Route[] = [
  {
    path: '/7guis',
    id: 'layout',
    children: [
      { path: '', id: 'index' },
      { path: ':task', id: 'task' },
    ],
  },
]

test('a nested match returns the whole chain, root first', () => {
  expect(hit(NESTED, '/7guis/cells')).toEqual(['layout', 'task'])
})

test('an index child wins over its parent matching alone', () => {
  // Both patterns are '/7guis'; the index child is the more specific way to
  // say "exactly here", so it must not lose on declaration order.
  expect(hit(NESTED, '/7guis')).toEqual(['layout', 'index'])
})

test('a parent with children can still match on its own', () => {
  const routes: Route[] = [{ path: '/a', id: 'parent', children: [{ path: 'b', id: 'child' }] }]
  expect(hit(routes, '/a')).toEqual(['parent'])
  expect(hit(routes, '/a/b')).toEqual(['parent', 'child'])
})

test('each depth reports the portion of the path it accounts for', () => {
  const matched = matchRoutes(flattenRoutes(NESTED), '/7guis/cells')
  expect(matched?.map((one) => one.pathname)).toEqual(['/7guis', '/7guis/cells'])
})

test('a pathless layout accounts for none of the path', () => {
  const routes: Route[] = [
    { path: '', id: 'shell', children: [{ path: 'todo', id: 'todo' }] },
  ]
  const matched = matchRoutes(flattenRoutes(routes), '/todo')
  expect(matched?.map((one) => one.route.id)).toEqual(['shell', 'todo'])
  expect(matched?.map((one) => one.pathname)).toEqual(['/', '/todo'])
})

test('every depth sees every param, including ones matched below it', () => {
  const routes: Route[] = [
    { path: '/:org', id: 'org', children: [{ path: 'repo/:name', id: 'repo' }] },
  ]
  const matched = matchRoutes(flattenRoutes(routes), '/acme/repo/site')
  expect(matched?.map((one) => one.params)).toEqual([
    { org: 'acme', name: 'site' },
    { org: 'acme', name: 'site' },
  ])
})

test('nesting goes as deep as it is declared', () => {
  const routes: Route[] = [
    { path: '/a', id: 'a', children: [{ path: 'b', id: 'b', children: [{ path: ':c', id: 'c' }] }] },
  ]
  expect(hit(routes, '/a/b/3')).toEqual(['a', 'b', 'c'])
})

// ---------------------------------------------------------------------------
// Bad patterns
// ---------------------------------------------------------------------------

test('a wildcard has to be last', () => {
  expect(() => flattenRoutes([{ path: '/a/*/b' }])).toThrow(/not the last segment/)
})

test('a wildcard in a parent has to be last across the whole chain', () => {
  // Legal in isolation, but the child appends segments after it.
  expect(() => flattenRoutes([{ path: '/a/*', children: [{ path: 'b' }] }])).toThrow(
    /not the last segment/,
  )
})

test('a parameter needs a name', () => {
  expect(() => flattenRoutes([{ path: '/a/:' }])).toThrow(/with no name after it/)
})

// ---------------------------------------------------------------------------
// joinPaths and buildPath
// ---------------------------------------------------------------------------

test('joining is indifferent to how the pieces are punctuated', () => {
  expect(joinPaths('/a', 'b')).toBe('/a/b')
  expect(joinPaths('/a/', '/b/')).toBe('/a/b')
  expect(joinPaths('', 'b')).toBe('/b')
  expect(joinPaths('/a', '')).toBe('/a')
  expect(joinPaths('', '')).toBe('/')
})

test('buildPath is the inverse of matching', () => {
  expect(buildPath('/users/:id', { id: '2' })).toBe('/users/2')
  expect(buildPath('/users/:id/edit', { id: '2' })).toBe('/users/2/edit')
  expect(buildPath('/todo')).toBe('/todo')
  expect(buildPath('/')).toBe('/')
})

test('buildPath escapes values so they survive the round trip', () => {
  const path = buildPath('/tags/:name', { name: 'two words' })
  expect(path).toBe('/tags/two%20words')
  expect(paramsOf([{ path: '/tags/:name' }], path)).toEqual({ name: 'two words' })
})

test('buildPath drops an absent optional and refuses an absent required one', () => {
  expect(buildPath('/posts/:page?', {})).toBe('/posts')
  expect(() => buildPath('/users/:id', {})).toThrow(/needs a "id" param/)
})

test('buildPath fills a wildcard from its name', () => {
  expect(buildPath('/files/*', { '*': 'a/b' })).toBe('/files/a/b')
  expect(buildPath('/files/*rest', { rest: 'a/b' })).toBe('/files/a/b')
  expect(buildPath('/files/*', {})).toBe('/files')
})

// ---------------------------------------------------------------------------
// parsePath and resolvePath
// ---------------------------------------------------------------------------

test('parsePath keeps each piece with its punctuation', () => {
  expect(parsePath('/a/b?x=1#top')).toEqual({ pathname: '/a/b', search: '?x=1', hash: '#top' })
  expect(parsePath('/a')).toEqual({ pathname: '/a', search: '', hash: '' })
  expect(parsePath('?x=1')).toEqual({ pathname: '/', search: '?x=1', hash: '' })
  // A `?` inside the fragment belongs to the fragment.
  expect(parsePath('/a#b?c')).toEqual({ pathname: '/a', search: '', hash: '#b?c' })
})

test('an absolute target ignores where it was written', () => {
  expect(resolvePath('/x', '/a/b')).toBe('/x')
  expect(resolvePath('/x?q=1#t', '/a/b')).toBe('/x?q=1#t')
})

test('a relative target resolves against the directory it sits in', () => {
  expect(resolvePath('c', '/a/b')).toBe('/a/c')
  expect(resolvePath('../c', '/a/b/d')).toBe('/a/c')
  expect(resolvePath('./c', '/a/b')).toBe('/a/c')
})

test('walking past the root stops at the root', () => {
  expect(resolvePath('../../../x', '/a')).toBe('/x')
  expect(resolvePath('..', '/a')).toBe('/')
})

test('a bare query or fragment stays on the current page', () => {
  expect(resolvePath('?arm=fine', '/particles?arm=coarse')).toBe('/particles?arm=fine')
  expect(resolvePath('#top', '/docs/intro')).toBe('/docs/intro#top')
  expect(resolvePath('', '/docs/intro')).toBe('/docs/intro')
})
