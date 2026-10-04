const GITHUB = 'https://github.com/agent-habilis/agent-gossip'
const DISCORD = 'https://discord.gg/7FrS8GkQ8'

// The landing page and the docs share one shell, so crossing between them
// moves the content and nothing around it.
export function SiteNav({ current }: { current?: 'docs' }) {
  return (
    <nav className="p-grid site-nav" data-columns="8">
      <div data-span="2" data-span-s="row">
        <a href="/" className="brand">
          <strong>agent-gossip 💬</strong>
        </a>
      </div>
      <div className="nav-links" data-span="3.." data-span-s="row">
        <a href="/docs-grid/" aria-current={current === 'docs' ? 'page' : undefined}>
          Docs
        </a>
        {/* The webapp opens in its own tab, as every link to it on the docs does. */}
        <a href="/app/" target="_blank" rel="noopener">
          Webapp
        </a>
        <a href={GITHUB}>GitHub</a>
        <a href={DISCORD}>Discord</a>
      </div>
    </nav>
  )
}

export interface SideNavItem {
  href: string
  label: string
  current?: boolean
}

export function SideNav({ title, items }: { title?: string; items: SideNavItem[] }) {
  return (
    <aside className="sidebar" data-span="2" data-span-s="row">
      {title && <h6>{title}</h6>}
      <ul>
        {items.map((item) => (
          <li key={item.href}>
            <a href={item.href} aria-current={item.current ? 'page' : undefined}>
              {item.label}
            </a>
          </li>
        ))}
      </ul>
    </aside>
  )
}
