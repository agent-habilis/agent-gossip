import { generateStaticParamsFor, importPage } from 'nextra/pages'

import { useMDXComponents as getMDXComponents } from '@/mdx-components'

const allParams = generateStaticParamsFor('mdxPath')

// The blog renders on the primitive shell under app/(docs-grid)/blog, not in
// the docs theme.
export async function generateStaticParams() {
  const params = await allParams()
  return params.filter(({ mdxPath }) => mdxPath?.[0] !== 'blog')
}

type Props = { params: Promise<{ mdxPath?: string[] }> }

export async function generateMetadata(props: Props) {
  const params = await props.params
  const { metadata } = await importPage(params.mdxPath)
  return metadata
}

const Wrapper = getMDXComponents().wrapper

export default async function Page(props: Props) {
  const params = await props.params
  const { default: MDXContent, ...page } = await importPage(params.mdxPath)
  return (
    <Wrapper {...page}>
      <MDXContent {...props} params={params} />
    </Wrapper>
  )
}
