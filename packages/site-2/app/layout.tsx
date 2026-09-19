import type { Metadata } from 'next'
import type { ReactNode } from 'react'
import { Footer, Layout, Navbar } from 'nextra-theme-docs'
import { Head } from 'nextra/components'
import { getPageMap } from 'nextra/page-map'

import 'nextra-theme-docs/style.css'
import './styles.css'

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

const FOOTER_LINKS = [
  ['GitHub', 'https://github.com/agent-habilis/agent-gossip'],
  ['Discord', 'https://discord.gg/7FrS8GkQ8'],
  ['Docs', '/docs'],
  ['License', 'https://github.com/agent-habilis/agent-gossip/blob/main/LICENSE'],
  ['agent-habilis', 'https://agent-habilis.com'],
  ['fofoca', 'https://github.com/fofoca-network/fofoca'],
  ['iroh', 'https://www.iroh.computer/'],
] as const

export default async function RootLayout({ children }: { children: ReactNode }) {
  const pageMap = await getPageMap()
  return (
    <html lang="en" dir="ltr" suppressHydrationWarning>
      <Head
        faviconGlyph="💬"
        color={{
          hue: { light: 213, dark: 210 },
          saturation: { light: 86, dark: 94 },
          lightness: { light: 42, dark: 67 },
        }}
      />
      <body>
        <Layout
          pageMap={pageMap}
          docsRepositoryBase="https://github.com/agent-habilis/agent-gossip/tree/main/packages/site-2"
          sidebar={{ defaultMenuCollapseLevel: 1 }}
          // The site follows the OS color scheme; there is no switch to pick one.
          darkMode={false}
          navbar={
            <Navbar
              logo={<b>agent-gossip 💬</b>}
              projectLink="https://github.com/agent-habilis/agent-gossip"
              chatLink="https://discord.gg/7FrS8GkQ8"
            />
          }
          footer={
            <Footer>
              <ul className="foot-links">
                {FOOTER_LINKS.map(([label, href]) => (
                  <li key={href}>
                    <a href={href} {...(href.startsWith('/') ? {} : { target: '_blank', rel: 'noopener' })}>
                      {label}
                    </a>
                  </li>
                ))}
              </ul>
            </Footer>
          }
        >
          {children}
        </Layout>
      </body>
    </html>
  )
}
