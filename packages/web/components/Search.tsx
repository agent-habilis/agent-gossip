'use client'
import { useEffect, useRef, useState } from 'react'

interface Hit {
  url: string
  title: string
  excerpt: string
}

interface Pagefind {
  search: (query: string) => Promise<{ results: { data: () => Promise<{ url: string; excerpt: string; meta: { title?: string } }> }[] }>
}

let loading: Promise<Pagefind> | undefined

// Loaded on first use, from the export's /_pagefind/: the URL is resolved by
// the browser, not the bundler. `next dev` has no index, so this rejects there.
function pagefind(): Promise<Pagefind> {
  loading ??= import(/* webpackIgnore: true */ /* turbopackIgnore: true */ '/_pagefind/pagefind.js' as string)
  return loading
}

export function Search() {
  const [query, setQuery] = useState('')
  const [hits, setHits] = useState<Hit[] | 'error'>([])
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  const input = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (!query.trim()) return
    let stale = false
    const timer = setTimeout(async () => {
      try {
        const { results } = await (await pagefind()).search(query)
        const data = await Promise.all(results.slice(0, 8).map((result) => result.data()))
        if (!stale) setHits(data.map((item) => ({ url: item.url, title: item.meta.title ?? item.url, excerpt: item.excerpt })))
      } catch {
        if (!stale) setHits('error')
      }
    }, 150)
    return () => {
      stale = true
      clearTimeout(timer)
    }
  }, [query])

  // Close on a press outside, not on blur: a click on a result must land first,
  // and some browsers (Safari) give a clicked link no focus at all.
  useEffect(() => {
    if (!open) return
    const close = (event: PointerEvent) => !root.current?.contains(event.target as Node) && setOpen(false)
    document.addEventListener('pointerdown', close)
    return () => document.removeEventListener('pointerdown', close)
  }, [open])

  return (
    <div className="search" ref={root}>
      <input
        ref={input}
        type="search"
        placeholder="Search docs"
        aria-label="Search docs"
        value={query}
        onChange={(event) => {
          setQuery(event.target.value)
          setOpen(true)
        }}
        onFocus={() => setOpen(true)}
        onKeyDown={(event) => event.key === 'Escape' && setOpen(false)}
      />
      {/* Our own clear button: the browser's sits on the right, under the
          right-aligned text. A text glyph, like every icon on the site. */}
      {query && (
        <button
          type="button"
          className="search-clear"
          aria-label="Clear search"
          onClick={() => {
            setQuery('')
            input.current?.focus()
          }}
        >
          ×
        </button>
      )}
      {open && query.trim() && (
        <div className="search-results" role="listbox">
          {hits === 'error' ? (
            <p>Search works on the built site.</p>
          ) : hits.length === 0 ? (
            <p>No results.</p>
          ) : (
            <ul>
              {hits.map((hit) => (
                <li key={hit.url}>
                  <a href={hit.url}>{hit.title}</a>
                  {/* Pagefind's own excerpt, from our own index: text with <mark>. */}
                  <p dangerouslySetInnerHTML={{ __html: hit.excerpt }} />
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  )
}
