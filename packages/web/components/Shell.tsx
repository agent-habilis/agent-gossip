import { Search } from './Search'

export const GITHUB = 'https://github.com/agent-habilis/agent-gossip'
const DISCORD = 'https://discord.gg/Y88YJwEeuZ'

const LINKS = [
  ['GitHub', GITHUB],
  ['Discord', DISCORD],
  ['Docs', '/docs'],
  ['License', `${GITHUB}/blob/main/LICENSE`],
] as const

const SIBLINGS = [
  ['agent-graph', 'https://github.com/agent-habilis/agent-graph'],
  ['agent-inject', 'https://github.com/agent-habilis/agent-inject'],
  ['agent-share', 'https://github.com/agent-habilis/agent-share'],
  ['habilis-network', 'https://github.com/agent-habilis/habilis-network'],
  ['agent-habilis.com', 'https://agent-habilis.com'],
] as const

// The landing page and the docs share one shell, so crossing between them
// moves the content and nothing around it.
export function SiteNav({ current }: { current?: 'home' | 'docs' | 'blog' }) {
  return (
    <nav className="p-grid site-nav">
      <div data-span="3" data-span-s="row">
        <a href="/" className="brand" aria-current={current === 'home' ? 'page' : undefined}>
          <strong>agent-gossip 💬</strong>
        </a>
      </div>
      <div className="nav-links" data-span="4-9" data-span-s="row">
        <a href="/docs/" aria-current={current === 'docs' ? 'page' : undefined}>
          Docs
        </a>
        <a href={GITHUB}>GitHub</a>
        <a href={DISCORD}>Discord</a>
      </div>
      {/* Over the docs' "On this page" column, at the page's right edge. */}
      {current === 'docs' && (
        <div className="nav-search" data-span="10.." data-span-s="row">
          <Search />
        </div>
      )}
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
    <aside className="sidebar" data-span="3" data-span-s="row">
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

function FooterColumn({ title, links }: { title: string; links: readonly (readonly [string, string])[] }) {
  return (
    <div data-span="6" data-span-s="row">
      <h6>{title}</h6>
      <p>
        {links.map(([label, href]) => (
          <span key={href}>
            <a href={href} target="_blank" rel="noopener noreferrer">
              {label}
            </a>
            <br />
          </span>
        ))}
      </p>
    </div>
  )
}

export function SiteFooter() {
  return (
    <footer className="p-grid site-footer">
      <hr />
      <FooterColumn title="Links" links={LINKS} />
      <FooterColumn title="agent-habilis █🫈" links={SIBLINGS} />
    </footer>
  )
}
