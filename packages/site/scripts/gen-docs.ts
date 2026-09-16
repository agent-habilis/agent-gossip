import { rm } from 'node:fs/promises'

export interface Page {
  /** Relative to content/docs/, e.g. `commands/create.mdx`. */
  path: string
  title: string
  /** MDX, without frontmatter. */
  body: string
}

const MANUAL = new URL('../../../docs/manual.txt', import.meta.url)
const OUT = new URL('../content/docs/', import.meta.url)

const SECTION = /^[A-Z][A-Z0-9 ]*$/
// A man-page tagged paragraph: the tag, two or more spaces, then its text.
const TAGGED = /^(\S+) {2,}(\S.*)$/
const BULLET = /^•\s+/
const ACRONYMS = new Set(['JSON', 'MCP', 'A2A'])
const OVERVIEW = ['NAME', 'SYNOPSIS', 'DESCRIPTION']
const REFERENCE = ['EXIT STATUS', 'ENVIRONMENT', 'FILES', 'CONVENTIONS', 'SEE ALSO']
const COMMAND_INDENT = 7
const COMMAND_BODY_INDENT = 14

const indentOf = (line: string) => line.length - line.trimStart().length
const isBlank = (line: string | undefined) => line === undefined || line.trim() === ''

function sentenceCase(caps: string): string {
  return caps
    .split(' ')
    .map((word, index) => {
      if (ACRONYMS.has(word)) return word
      const lower = word.toLowerCase()
      return index === 0 ? lower.charAt(0).toUpperCase() + lower.slice(1) : lower
    })
    .join(' ')
}

// The manual is plain text, so anything MDX or GFM would read as syntax has to
// be escaped — `<nick>` would open a JSX tag and `~/.claude … ~10s` would strike
// through. Backtick spans are left alone: they are already code.
function escapeProse(text: string): string {
  const escaped = text
    .split(/(`[^`]*`)/)
    .map((part, index) => (index % 2 ? part : part.replace(/[\\<>{}~]/g, '\\$&')))
    .join('')
  return /^([#+-]|\d+[.)])\s/.test(escaped) ? `\\${escaped}` : escaped
}

function heading(depth: number, markdown: string): string {
  return `${'#'.repeat(Math.min(2 + depth, 4))} ${markdown}`
}

function termHeading(depth: number, term: string): string {
  const isCode = /^-|^[\w.()-]+( \/ [\w.()-]+)*$/.test(term)
  return heading(depth, isCode ? `\`${term}\`` : escapeProse(term))
}

function codeBlock(lines: string[], lang = 'text'): string {
  const indent = Math.min(...lines.filter((line) => !isBlank(line)).map(indentOf))
  const body = lines.map((line) => (isBlank(line) ? '' : line.slice(indent).trimEnd()))
  return ['```' + lang, ...body, '```'].join('\n')
}

function bulletList(lines: string[]): string {
  const items: string[] = []
  for (const line of lines) {
    if (isBlank(line)) continue
    const text = line.trim()
    if (BULLET.test(text) || items.length === 0) items.push(text.replace(BULLET, ''))
    else items[items.length - 1] += ` ${text}`
  }
  return items.map((item) => `- ${escapeProse(item)}`).join('\n')
}

/**
 * The end of the run of lines from `from` that sit deeper than `indent`. Blank
 * lines inside the run are kept when `allowBlank`, trailing ones never are.
 */
function deeperRun(lines: string[], from: number, indent: number, allowBlank: boolean): number {
  let end = from
  for (let index = from; index < lines.length; index++) {
    const line = lines[index]!
    if (isBlank(line)) {
      if (!allowBlank) break
      continue
    }
    if (indentOf(line) <= indent) break
    end = index + 1
  }
  return end
}

/**
 * Renders one indentation level. `top` is true for a section or command body:
 * there a tagged row is a heading (the JSON event names, the global options),
 * while nested inside a term it is a field in a list.
 */
function render(lines: string[], depth: number, top: boolean): string[] {
  const first = lines.find((line) => !isBlank(line))
  if (first === undefined) return []
  const level = indentOf(first)

  const blocks: string[] = []
  let prose: string[] = []
  const flush = () => {
    if (prose.length > 0) blocks.push(escapeProse(prose.join(' ')))
    prose = []
  }

  let index = 0
  while (index < lines.length) {
    const line = lines[index]!
    if (isBlank(line)) {
      flush()
      index++
      continue
    }
    const indent = indentOf(line)
    const text = line.trim()

    // Deeper than the prose around it, and not a term's body: an example, or a list.
    if (indent > level) {
      flush()
      const end = deeperRun(lines, index, level, true)
      const run = lines.slice(index, end)
      blocks.push(BULLET.test(text) ? bulletList(run) : codeBlock(run))
      index = end
      continue
    }

    if (SECTION.test(text)) {
      flush()
      const end = deeperRun(lines, index + 1, indent, true)
      blocks.push(heading(depth, escapeProse(sentenceCase(text))))
      blocks.push(...render(lines.slice(index + 1, end), depth + 1, false))
      index = end
      continue
    }

    if (BULLET.test(text)) {
      flush()
      let end = index
      while (end < lines.length && indentOf(lines[end]!) === level && BULLET.test(lines[end]!.trim())) {
        end = deeperRun(lines, end + 1, level, false)
      }
      blocks.push(bulletList(lines.slice(index, end)))
      index = end
      continue
    }

    // A term starts a paragraph; mid-paragraph, a deeper line is an example.
    const hasBody = !isBlank(lines[index + 1]) && indentOf(lines[index + 1]!) > indent
    const tagged = TAGGED.exec(text)
    if (prose.length === 0 && tagged && !top) {
      const items: string[] = []
      let end = index
      let row: RegExpExecArray | null
      while (end < lines.length && indentOf(lines[end]!) === level && (row = TAGGED.exec(lines[end]!.trim()))) {
        const next = deeperRun(lines, end + 1, level, false)
        const continuation = lines.slice(end + 1, next).map((part) => part.trim())
        items.push(`- \`${row[1]}\` — ${escapeProse([row[2], ...continuation].join(' '))}`)
        end = next
      }
      blocks.push(items.join('\n'))
      index = end
      continue
    }
    if (prose.length === 0 && (tagged || hasBody)) {
      const end = deeperRun(lines, index + 1, indent, true)
      const body = lines.slice(index + 1, end)
      if (tagged) {
        const bodyIndent = hasBody ? indentOf(lines[index + 1]!) : indent + 1
        body.unshift(' '.repeat(bodyIndent) + tagged[2])
      }
      blocks.push(termHeading(depth, tagged ? tagged[1]! : text))
      blocks.push(...render(body, depth + 1, false))
      index = end
      continue
    }

    prose.push(text)
    index++
  }
  flush()
  return blocks
}

function splitSections(manual: string): { title: string; lines: string[] }[] {
  const sections: { title: string; lines: string[] }[] = []
  // The first and last lines are the man page's running header and footer.
  for (const line of manual.trimEnd().split('\n').slice(1, -1)) {
    if (SECTION.test(line)) sections.push({ title: line, lines: [] })
    else sections.at(-1)?.lines.push(line)
  }
  return sections
}

function splitCommands(lines: string[]): { header: string[]; body: string[] }[] {
  const entries: { header: string[]; body: string[] }[] = []
  let inHeader = false
  for (const line of lines) {
    if (!inHeader && !isBlank(line) && indentOf(line) === COMMAND_INDENT) {
      entries.push({ header: [line], body: [] })
      inHeader = true
      continue
    }
    const entry = entries.at(-1)
    if (!entry) continue
    // `poll` and `state` have two header lines, `a2a call` wraps its header;
    // the header ends where the description starts.
    if (inHeader && !isBlank(line) && indentOf(line) !== COMMAND_BODY_INDENT) {
      entry.header.push(line)
      continue
    }
    inHeader = false
    entry.body.push(line)
  }
  return entries
}

function commandPage(entry: { header: string[]; body: string[] }): Page {
  const first = entry.header[0]!.trim()
  const body = render(entry.body, 0, true)

  // TASKS and ORCHESTRATION are concepts filed among the commands.
  if (SECTION.test(first)) {
    return { path: `commands/${first.toLowerCase()}.mdx`, title: sentenceCase(first), body: body.join('\n\n') }
  }

  const names = [
    ...new Set(
      entry.header
        .filter((line) => indentOf(line) === COMMAND_INDENT)
        .map((line) => {
          const words = line.trim().split(' ')
          const end = words.findIndex((word) => !/^[a-z0-9]+$/.test(word))
          return words.slice(0, end === -1 ? undefined : end).join(' ')
        }),
    ),
  ]
  const slug = names.length === 1 ? names[0]!.replaceAll(' ', '-') : names[0]!.split(' ')[0]!
  const synopsis = ['```sh', ...entry.header.map((line) => line.slice(COMMAND_INDENT).trimEnd()), '```'].join('\n')
  return { path: `commands/${slug}.mdx`, title: names.join(' / '), body: [synopsis, ...body].join('\n\n') }
}

export function generate(manual: string): Page[] {
  const pages: Page[] = []
  let overview: Page | undefined
  let reference: Page | undefined

  for (const { title, lines } of splitSections(manual)) {
    if (title === 'COMMANDS') {
      const commands = splitCommands(lines).map(commandPage)
      const links = commands.map((page) => `- [${page.title}](/docs/${page.path.replace(/\.mdx$/, '')}/)`)
      pages.push({ path: 'commands/index.mdx', title: 'Commands', body: links.join('\n') }, ...commands)
      continue
    }

    const grouped = OVERVIEW.includes(title) || REFERENCE.includes(title)
    const content =
      title === 'SYNOPSIS' ? [codeBlock(lines, 'sh')] : render(lines, grouped ? 1 : 0, true)

    if (!grouped) {
      const slug = title.toLowerCase().replaceAll(' ', '-')
      pages.push({ path: `${slug}.mdx`, title: sentenceCase(title), body: content.join('\n\n') })
      continue
    }

    let page = OVERVIEW.includes(title) ? overview : reference
    if (!page) {
      page = OVERVIEW.includes(title)
        ? (overview = { path: 'index.mdx', title: 'Overview', body: '' })
        : (reference = { path: 'reference.mdx', title: 'Reference', body: '' })
      pages.push(page)
    }
    page.body = [page.body, heading(0, sentenceCase(title)), ...content].filter(Boolean).join('\n\n')
  }
  return pages
}

/** Frontmatter on every page, plus the meta.json files that fix sidebar order to the manual's. */
export function toFiles(pages: Page[]): Map<string, string> {
  const files = new Map<string, string>()
  const root: string[] = []
  const folders = new Map<string, string[]>()

  for (const page of pages) {
    files.set(page.path, `---\ntitle: ${JSON.stringify(page.title)}\n---\n\n${page.body}\n`)
    const name = page.path.replace(/\.mdx$/, '')
    const slash = name.indexOf('/')
    if (slash === -1) {
      root.push(name)
      continue
    }
    const folder = name.slice(0, slash)
    if (!folders.has(folder)) {
      folders.set(folder, [])
      root.push(folder)
    }
    const child = name.slice(slash + 1)
    // The folder's own link already is its index page.
    if (child !== 'index') folders.get(folder)!.push(child)
  }

  files.set('meta.json', JSON.stringify({ pages: root }, null, 2))
  for (const [folder, names] of folders) {
    const title = pages.find((page) => page.path === `${folder}/index.mdx`)?.title
    files.set(`${folder}/meta.json`, JSON.stringify({ title, pages: names }, null, 2))
  }
  return files
}

if (import.meta.main) {
  const files = toFiles(generate(await Bun.file(MANUAL).text()))
  await rm(OUT, { recursive: true, force: true })
  await Promise.all([...files].map(([path, content]) => Bun.write(new URL(path, OUT), content)))
  console.log(`wrote ${files.size} files to content/docs/`)
}
