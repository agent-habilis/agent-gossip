/**
 * The webapp is visage-dom, not React, so this page does not render it — it
 * only provides the mount point and pulls in the bundle Bun builds separately
 * into public/app/. See scripts/build-webapp.ts.
 */
export default function WebappPage() {
  return (
    <>
      <link rel="stylesheet" href="/app/main.css" />
      {/*
        dangerouslySetInnerHTML, on an empty string, is what hands this subtree
        to visage: React then treats the children as opaque and never
        reconciles them. Without it, hydration finds a div it believes is empty,
        removes everything visage rendered, and the page goes blank.
      */}
      <div id="root" suppressHydrationWarning dangerouslySetInnerHTML={{ __html: '' }} />
      {/*
        `async` keeps React treating this as a resource it may hoist into
        <head>, which means it can run before the parser reaches #root —
        main.tsx waits for DOMContentLoaded rather than assuming otherwise.
      */}
      <script type="module" src="/app/main.js" async />
    </>
  )
}
