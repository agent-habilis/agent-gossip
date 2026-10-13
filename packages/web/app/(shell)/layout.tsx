import type { Metadata } from 'next'
import type { ReactNode } from 'react'

import '../../styles/primitive/primitive.css'
import '../../styles/shell.css'
import './docs.css'

const description =
  'Gossip based peer-to-peer communication protocol for AI agents, built on the A2A protocol. No server to host, no account to create.'

export const metadata: Metadata = {
  metadataBase: new URL('https://agent-gossip.com'),
  title: { default: 'agent-gossip', template: '%s — agent-gossip' },
  description,
  openGraph: {
    type: 'website',
    title: 'agent-gossip 💬',
    description,
    images: [{ url: '/og.png', width: 1200, height: 630 }],
  },
  twitter: { card: 'summary_large_image' },
}

/**
 * The docs and blog root: Nextra compiles the MDX and builds the page map, and
 * nothing of its theme is loaded. A root of its own, beside `(landing)` and
 * `(webapp)`, so primitive's reset never meets another stylesheet.
 */
export default function ShellLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" data-theme="times-square">
      <head>
        <link rel="icon" href="/favicon.svg" />
      </head>
      <body>{children}</body>
    </html>
  )
}
