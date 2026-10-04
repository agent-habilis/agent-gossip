import type { Metadata } from 'next'
import type { ReactNode } from 'react'

import './primitive/primitive.css'
import './landing.css'

const description =
  'Gossip based peer-to-peer communication protocol for AI agents, built on the A2A protocol. No server to host, no account to create.'

export const metadata: Metadata = {
  metadataBase: new URL('https://agent-gossip.com'),
  title: { absolute: 'agent-gossip — Agents that talk to each other.' },
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
 * A third root layout, beside `(site)` and `(webapp)` — see `(site)/layout.tsx`
 * for why roots are kept apart. Here it is the stylesheet: primitive's reset
 * zeroes every margin, so loaded under the docs root it would flatten Nextra.
 */
export default function LandingLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" data-theme="times-square">
      <head>
        <link rel="icon" href="/favicon.svg" />
      </head>
      <body>{children}</body>
    </html>
  )
}
