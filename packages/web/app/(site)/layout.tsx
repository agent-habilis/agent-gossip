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

/**
 * One of **two** root layouts; `(webapp)` is the other. Two roots rather than a
 * shared shell is what makes Next do a full document load when crossing between
 * the docs and the webapp — and that load is load-bearing: the webapp is a
 * visage app mounted by a plain module script, which evaluates once per
 * document. Under a shared root the crossing is a soft navigation, and a second
 * one leaves the mount point empty, the docs stylesheet applied over the app,
 * and the previous wasm client running on a detached tree.
 */
export default async function SiteLayout({ children }: { children: ReactNode }) {
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
          docsRepositoryBase="https://github.com/agent-habilis/agent-gossip/tree/main/packages/web"
          sidebar={{ defaultMenuCollapseLevel: 1, toggleButton: false }}
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
        {/*
          The webapp opens in its own tab. Every link in the content says so in
          its own markup; the navbar's cannot, because it comes from
          `content/_meta.ts`, and a page item there carries a title and an href
          and nothing else. Without JavaScript the link still works — it opens
          in the same tab.
        */}
        <script
          dangerouslySetInnerHTML={{
            __html: `document.querySelectorAll('a[href^="/app"]').forEach(function(a){a.target="_blank";a.rel="noopener"})`,
          }}
        />
      </body>
    </html>
  )
}
