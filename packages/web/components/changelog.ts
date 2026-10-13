import { readFile } from 'node:fs/promises'
import { join } from 'node:path'

const PAGE = '/docs/changelog/'

/**
 * Where the changelog page lists a release. Its heading is "0.11.3
 * (2026-09-30)", so the anchor carries the date and has to be read from the
 * file. A version not released yet has no heading, so it links to the page.
 */
export async function changelogHref(version: string): Promise<string> {
  const changelog = await readFile(join(process.cwd(), '../../CHANGELOG.md'), 'utf8')
  const escaped = version.replaceAll('.', '\\.')
  const heading = new RegExp(`^## (${escaped} \\([^)]*\\))`, 'm').exec(changelog)?.[1]
  if (!heading) return PAGE
  // The same slug the MDX heading gets: lowercase, punctuation dropped,
  // spaces turned into hyphens.
  const slug = heading.toLowerCase().replace(/[^a-z0-9 -]/g, '').replaceAll(' ', '-')
  return `${PAGE}#${slug}`
}
