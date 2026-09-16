import type { BaseLayoutProps } from 'fumadocs-ui/layouts/shared'

export function baseOptions(): BaseLayoutProps {
  return {
    nav: { title: 'agent-gossip 💬' },
    links: [
      { text: 'Docs', url: '/docs/' },
      { text: 'Discord', url: 'https://discord.gg/7FrS8GkQ8', external: true },
    ],
    githubUrl: 'https://github.com/agent-habilis/agent-gossip',
  }
}
