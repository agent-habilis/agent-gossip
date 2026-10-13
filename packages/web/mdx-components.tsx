import { PLAIN_COMPONENTS } from '@/components/mdxComponents'

export function useMDXComponents(components?: Record<string, unknown>) {
  return { ...PLAIN_COMPONENTS, ...components }
}
