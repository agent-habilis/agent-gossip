import type { Metadata } from 'next'
import type { ReactNode } from 'react'

export const metadata: Metadata = {
  metadataBase: new URL('https://agent-gossip.com'),
  // Absolute: this root has no title template, and the webapp is not "a page
  // on the site" — it is the product.
  title: { absolute: 'agent-gossip' },
  // A client-rendered shell with nothing to index, and every room behind it is
  // private.
  robots: { index: false, follow: false },
}

/**
 * The second root layout — see `(site)/layout.tsx` for why there are two.
 * Deliberately bare: no navbar, no sidebar, no docs stylesheet. The webapp is a
 * full-viewport application, and anything the docs chrome puts on the page is
 * something it has to fight.
 */
export default function WebappLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" dir="ltr">
      <head>
        <link rel="icon" href="/favicon.svg" />
        {/*
          First-paint guard: pick the light/dark half of the chrome before any
          JavaScript runs, so a hard reload never flashes the wrong colors. The
          app paints on --bg-sunken (see webapp/app.css), hence that pair here.
          color-scheme goes on html only — on body it would pin the UA's own
          rendering.
        */}
        <style>{`
          html {
            color-scheme: light dark;
            background: light-dark(#f6f6f6, #1e1d1e);
            color: light-dark(#272727, #ffffff);
          }
        `}</style>
      </head>
      <body>{children}</body>
    </html>
  )
}
