import type { ReactNode } from 'react'

import { useMDXComponents as getNextraComponents } from 'nextra/mdx-components'

// Plain elements, styled by primitive like the landing page; the wrapper is a
// no-op so the page around the MDX is ours. mdx-components.tsx hands these to
// every compiled page.
const PLAIN = ['h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'p', 'ul', 'ol', 'li', 'blockquote', 'hr', 'pre', 'code',
  'table', 'thead', 'tbody', 'tr', 'th', 'td', 'details', 'summary']

export const PLAIN_COMPONENTS = {
  ...getNextraComponents(),
  ...Object.fromEntries(PLAIN.map((tag) => [tag, tag])),
  wrapper: ({ children }: { children: ReactNode }) => children,
}
