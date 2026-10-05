import type { ReactNode } from 'react'

import { useMDXComponents as getNextraComponents } from 'nextra/mdx-components'

// The compiled MDX also pulls the docs theme's components from
// mdx-components.tsx — its wrapper alone brings the theme's TOC and layout.
// Plain elements instead, styled by primitive like the landing page.
const PLAIN = ['h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'p', 'ul', 'ol', 'li', 'blockquote', 'hr', 'pre', 'code',
  'table', 'thead', 'tbody', 'tr', 'th', 'td', 'details', 'summary']

export const PLAIN_COMPONENTS = {
  ...getNextraComponents(),
  ...Object.fromEntries(PLAIN.map((tag) => [tag, tag])),
  wrapper: ({ children }: { children: ReactNode }) => children,
}
