import { createMDX } from 'fumadocs-mdx/next'

const withMDX = createMDX()

/** @type {import('next').NextConfig} */
const config = {
  // Built into server/dist/ and served by server.ts, which has no Node to run
  // Next with. trailingSlash writes docs/x/index.html rather than docs/x.html,
  // the shape server.ts already maps `/x/` onto.
  output: 'export',
  trailingSlash: true,
  reactStrictMode: true,
}

export default withMDX(config)
