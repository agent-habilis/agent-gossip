import { generateStaticParamsFor, importPage } from 'nextra/pages'

import { useMDXComponents as getMDXComponents } from '@/mdx-components'

export const generateStaticParams = generateStaticParamsFor('mdxPath')

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
