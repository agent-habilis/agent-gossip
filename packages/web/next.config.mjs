import nextra from 'nextra'

const withNextra = nextra({})

/** @type {import('next').NextConfig} */
const config = {
  // Built into out/ and served by server.ts, which has no Node to run
  // Next with. trailingSlash writes docs/x/index.html rather than docs/x.html,
  // the shape server.ts already maps `/x/` onto.
  output: 'export',
  trailingSlash: true,
  // No image optimizer in a static export.
  images: { unoptimized: true },
  reactStrictMode: true,
}

export default withNextra(config)
