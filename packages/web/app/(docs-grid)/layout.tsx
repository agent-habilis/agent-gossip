import type { Metadata } from 'next'
import type { ReactNode } from 'react'

import '../../styles/primitive/primitive.css'
import '../../styles/shell.css'
import './docs-grid.css'

export const metadata: Metadata = {
  metadataBase: new URL('https://agent-gossip.com'),
  title: { default: 'agent-gossip', template: '%s — agent-gossip' },
  // A prototype beside /docs, rendering the same pages: indexing it would
  // split search results between two copies.
  robots: { index: false, follow: false },
}

/**
 * The docs prototype on primitive's grid, from scratch: Nextra compiles the MDX
 * and nothing else. A root of its own for the reason `(landing)` has one —
 * primitive's reset must never meet the Nextra theme's styles.
 */
export default function DocsGridLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" data-theme="times-square">
      <head>
        <link rel="icon" href="/favicon.svg" />
      </head>
      <body>{children}</body>
    </html>
  )
}
