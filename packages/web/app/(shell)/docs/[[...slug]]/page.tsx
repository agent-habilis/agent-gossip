import { importPage } from 'nextra/pages'

import { SideNav, SiteNav } from '@/components/Shell'
import meta from '@/content/docs/_meta'

const BASE = '/docs'

// The sidebar's order and titles are the docs' own _meta.
const PAGES = Object.entries(meta).map(([key, title]) => ({
  slug: key === 'index' ? '' : key,
  title,
}))

const href = (slug: string) => (slug ? `${BASE}/${slug}/` : `${BASE}/`)

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

export default async function DocsPage(props: Props) {
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

        <article data-span="4-9" data-span-s="row" data-pagefind-body>
          <MDXContent {...props} />

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
