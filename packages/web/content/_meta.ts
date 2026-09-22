export default {
  index: {
    type: 'page',
    title: 'Home',
    display: 'hidden',
    theme: {
      layout: 'full',
      sidebar: false,
      toc: false,
      breadcrumb: false,
      pagination: false,
      timestamp: false,
      copyPage: false,
    },
  },
  docs: { type: 'page', title: 'Docs' },
  // The webapp is a Next route, not MDX, so Nextra only knows its folder name.
  // Naming it here puts it in the navbar on purpose, rather than leaving an
  // item called "App" to appear in the docs sidebar as a side effect of the
  // page map. Crossing into it is a full document load — see (site)/layout.tsx.
  app: { type: 'page', title: 'Open the app', href: '/app/' },
}
