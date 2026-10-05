import type { Metadata } from 'next'
import type { ReactNode } from 'react'

import '../../styles/primitive/primitive.css'
import '../../styles/shell.css'
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
 * One of three root layouts, beside `(shell)` and `(webapp)`. Separate roots
 * make crossing between them a full document load, which the webapp needs and
 * which keeps each root's stylesheets to itself.
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
