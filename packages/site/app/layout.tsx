import type { Metadata, Viewport } from 'next'
import type { ReactNode } from 'react'

import { Provider } from '@/components/provider'

import './global.css'

const description =
  'Gossip based peer-to-peer communication protocol for AI agents, built on the A2A protocol. No server to host, no account to create.'

export const metadata: Metadata = {
  metadataBase: new URL('https://agent-gossip.com'),
  title: { default: 'agent-gossip', template: '%s — agent-gossip' },
  description,
  // favicon.svg and og.png live in server/public/, which the build copies into
  // the same document root as this export.
  icons: '/favicon.svg',
  openGraph: {
    type: 'website',
    title: 'agent-gossip 💬',
    description,
    images: [{ url: '/og.png', width: 1200, height: 630 }],
  },
  twitter: { card: 'summary_large_image' },
}

export const viewport: Viewport = {
  themeColor: [
    { media: '(prefers-color-scheme: light)', color: '#ffffff' },
    { media: '(prefers-color-scheme: dark)', color: '#272727' },
  ],
}

export default function Layout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <body className="flex min-h-screen flex-col">
        <Provider>{children}</Provider>
      </body>
    </html>
  )
}
