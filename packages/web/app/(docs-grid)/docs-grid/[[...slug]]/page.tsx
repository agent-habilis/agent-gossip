import { importPage } from 'nextra/pages'
import type { ReactNode } from 'react'

import { useMDXComponents as getNextraComponents } from 'nextra/mdx-components'

import { SideNav, SiteNav } from '@/components/Shell'
import meta from '@/content/docs/_meta'

const BASE = '/docs-grid'

// The sidebar's order and titles are the docs' own _meta, so the prototype
// cannot drift from /docs.
const PAGES = Object.entries(meta).map(([key, title]) => ({
  slug: key === 'index' ? '' : key,
  title,
}))

const href = (slug: string) => (slug ? `${BASE}/${slug}/` : `${BASE}/`)

// The compiled MDX also pulls the docs theme's components from
// mdx-components.tsx — its wrapper alone brings the theme's TOC and layout.
// Plain elements instead, styled by primitive like the landing page.
const PLAIN = ['h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'p', 'ul', 'ol', 'li', 'blockquote', 'hr', 'pre', 'code',
  'table', 'thead', 'tbody', 'tr', 'th', 'td', 'details', 'summary']
const COMPONENTS = {
  ...getNextraComponents(),
  ...Object.fromEntries(PLAIN.map((tag) => [tag, tag])),
  wrapper: ({ children }: { children: ReactNode }) => children,
}

type Props = { params: Promise<{ slug?: string[] }> }

export function generateStaticParams() {
  return PAGES.map(({ slug }) => ({ slug: slug ? [slug] : [] }))
}

export async function generateMetadata(props: Props) {
  const { slug = [] } = await props.params
  const { metadata } = await importPage(['docs', ...slug])
  return metadata
}

type TocItem = { id: string; value: unknown; depth: number }

export default async function DocsGridPage(props: Props) {
  const { slug = [] } = await props.params
  const current = slug.join('/')
  const { default: MDXContent, toc } = await importPage(['docs', ...slug])
  const index = PAGES.findIndex((page) => page.slug === current)
  const prev = PAGES[index - 1]
  const next = PAGES[index + 1]
  const headings = (toc as TocItem[]).filter((item) => item.depth === 2)

  return (
    <>
      <SiteNav current="docs" />

      <div className="p-grid docs">
        <SideNav
          title="Docs"
          items={PAGES.map((page) => ({ href: href(page.slug), label: page.title, current: page.slug === current }))}
        />

        <article data-span="4-9" data-span-s="row">
          <MDXContent {...props} components={COMPONENTS} />

          <hr />
          <p className="pager">
            {prev ? (
              <a className="p-button" data-variant="outline" href={href(prev.slug)}>
                ← {prev.title}
              </a>
            ) : (
              <span />
            )}
            {next && (
              <a className="p-button" data-variant="accent" data-icon="→" href={href(next.slug)}>
                {next.title}
              </a>
            )}
          </p>
        </article>

        {headings.length > 0 && (
          <aside className="toc" data-span="10.." data-span-s="row">
            <h6>On this page</h6>
            <ul>
              {headings.map((item) => (
                <li key={item.id}>
                  <a href={`#${item.id}`}>{item.value as string}</a>
                </li>
              ))}
            </ul>
          </aside>
        )}
      </div>
    </>
  )
}
