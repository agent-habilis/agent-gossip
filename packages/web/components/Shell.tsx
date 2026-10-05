export const GITHUB = 'https://github.com/agent-habilis/agent-gossip'
const DISCORD = 'https://discord.gg/7FrS8GkQ8'

const LINKS = [
  ['GitHub', GITHUB],
  ['Discord', DISCORD],
  ['Docs', '/docs'],
  ['License', `${GITHUB}/blob/main/LICENSE`],
  ['fofoca', 'https://github.com/fofoca-network/fofoca'],
] as const

const SIBLINGS = ['agent-browse', 'agent-file', 'agent-graph', 'agent-inject', 'agent-port', 'agent-share'].map(
  (name) => [name, `https://github.com/agent-habilis/${name}`] as const,
)

// The landing page and the docs share one shell, so crossing between them
// moves the content and nothing around it.
export function SiteNav({ current }: { current?: 'docs' | 'blog' }) {
  return (
    <nav className="p-grid site-nav">
      <div data-span="3" data-span-s="row">
        <a href="/" className="brand">
          <strong>agent-gossip 💬</strong>
        </a>
      </div>
      <div className="nav-links" data-span="4.." data-span-s="row">
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
            <a href={href}>{label}</a>
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
