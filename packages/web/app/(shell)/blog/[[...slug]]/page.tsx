import { notFound } from 'next/navigation'
import { importPage } from 'nextra/pages'

import { getPosts, postHref } from '@/components/blog'
import { SideNav, SiteNav } from '@/components/Shell'

type Props = { params: Promise<{ slug?: string[] }> }

export async function generateStaticParams() {
  const posts = await getPosts()
  return [{ slug: [] }, ...posts.map(({ slug }) => ({ slug: [slug] }))]
}

// /blog/ itself shows the latest post: a static export has no redirects, and
// the nav links straight to the post anyway.
async function resolve(props: Props) {
  const { slug = [] } = await props.params
  const posts = await getPosts()
  const index = slug.length ? posts.findIndex((post) => post.slug === slug[0]) : 0
  // An unknown post is a 404; an empty /blog/ with no posts yet renders empty.
  if (index === -1) notFound()
  return { posts, index, post: posts[index] }
}

export async function generateMetadata(props: Props) {
  const { post } = await resolve(props)
  return { title: post?.title }
}

export default async function BlogPage(props: Props) {
  const { posts, index, post } = await resolve(props)
  if (!post) return null
  const { default: MDXContent } = await importPage(['blog', post.slug])
  const newer = posts[index - 1]
  const older = posts[index + 1]

  return (
    <>
      <SiteNav current="blog" />

      <div className="p-grid docs">
        <SideNav
          title="Blog"
          items={posts.map((item) => ({ href: postHref(item.slug), label: item.title, current: item.slug === post.slug }))}
        />

        <article data-span="4-9" data-span-s="row">
          <h1>{post.title}</h1>
          <p>
            <small>
              <time dateTime={post.date}>{post.date}</time>
            </small>
          </p>
          <MDXContent {...props} />

          <hr />
          <p className="pager">
            {older ? (
              <a className="p-button" data-variant="outline" href={postHref(older.slug)}>
                ← {older.title}
              </a>
            ) : (
              <span />
            )}
            {newer && (
              <a className="p-button" data-variant="accent" data-icon="→" href={postHref(newer.slug)}>
                {newer.title}
              </a>
            )}
          </p>
        </article>
      </div>
    </>
  )
}
