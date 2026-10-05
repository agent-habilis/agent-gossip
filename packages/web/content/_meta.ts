export default {
  docs: { type: 'page', title: 'Docs' },
  // The webapp is a Next route, not MDX, so Nextra only knows its folder name.
  // Named here and hidden, so it is out of the navbar and no item called "App"
  // appears in the docs sidebar as a side effect of the page map. The route
  // still serves at /app/.
  app: { type: 'page', title: 'Webapp', href: '/app/', display: 'hidden' },
}
