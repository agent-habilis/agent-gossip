import { getPageMap } from 'nextra/page-map'

export interface Post {
  slug: string
  title: string
  date: string
}

/**
 * Every post under content/blog, newest first. The first post also needs
 * `blog: { display: 'hidden' }` in content/_meta.ts, or the /docs navbar lists
 * the folder; Nextra rejects that key while the folder does not exist.
 */
export async function getPosts(): Promise<Post[]> {
  // No content/blog folder yet means no page map for /blog: no posts.
  const items = await getPageMap('/blog').catch(() => [])
  return items
    .flatMap((item) => {
      if (!('frontMatter' in item) || !item.frontMatter?.date) return []
      const { title, date } = item.frontMatter as { title?: string; date: string }
      return [{ slug: item.name, title: title ?? item.name, date: String(date).slice(0, 10) }]
    })
    .sort((a, b) => b.date.localeCompare(a.date))
}

export const postHref = (slug: string) => `/blog/${slug}/`
