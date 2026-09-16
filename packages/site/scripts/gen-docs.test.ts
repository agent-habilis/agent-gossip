import { describe, expect, test } from 'bun:test'

import { evaluate } from '@mdx-js/mdx'
import { createElement } from 'react'
import * as runtime from 'react/jsx-runtime'
import { renderToStaticMarkup } from 'react-dom/server'
import remarkGfm from 'remark-gfm'

import { generate, type Page, toFiles } from './gen-docs.ts'

const manual = await Bun.file(new URL('../../../docs/manual.txt', import.meta.url)).text()
const pages = generate(manual)

async function renderText(page: Page): Promise<string> {
  const { default: Content } = await evaluate(page.body, { ...runtime, remarkPlugins: [remarkGfm] })
  // Inline tags join their neighbours (`PATH</code>:` reads as `PATH:`); block
  // tags separate them.
  return renderToStaticMarkup(createElement(Content))
    .replace(/<\/?(code|strong|em|a|span)\b[^>]*>/g, '')
    .replace(/<[^>]+>/g, ' ')
    .replaceAll('&lt;', '<')
    .replaceAll('&gt;', '>')
    .replaceAll('&quot;', '"')
    .replaceAll('&#x27;', "'")
    .replaceAll('&amp;', '&')
}

// Backticks, asterisks and the manual's • bullets are markup — the bullets
// become list items — so they are compared as absent on both sides.
const normalize = (text: string) => text.replace(/[`*•]/g, '').toLowerCase()

describe('generate', () => {
  test('keeps every word of the manual, in order', async () => {
    const lines = manual.trimEnd().split('\n')
    // The first and last lines are the man page's running header and footer.
    const words = normalize(lines.slice(1, -1).join('\n')).split(/\s+/).filter(Boolean)
    const rendered = normalize(
      (await Promise.all(pages.map(async (page) => `${page.title} ${await renderText(page)}`))).join(' '),
    )

    let at = 0
    for (const word of words) {
      const found = rendered.indexOf(word, at)
      if (found === -1) {
        throw new Error(`"${word}" is missing after: …${rendered.slice(Math.max(0, at - 120), at)}`)
      }
      at = found + word.length
    }
    expect(words.length).toBeGreaterThan(5000)
  })

  test('puts each command header in exactly one command page synopsis', () => {
    const commands = manual.slice(manual.indexOf('\nCOMMANDS\n'), manual.indexOf('\nGLOBAL OPTIONS\n'))
    const headers = commands.split('\n').filter((line) => /^ {7}[a-z]/.test(line))
    const synopses = pages
      .filter((page) => page.path.startsWith('commands/'))
      .map((page) => /^```sh\n([\s\S]*?)\n```/.exec(page.body)?.[1]?.split('\n') ?? [])

    expect(headers.length).toBeGreaterThan(25)
    for (const header of headers) {
      const hits = synopses.filter((lines) => lines.includes(header.trim()))
      expect({ header, hits: hits.length }).toEqual({ header, hits: 1 })
    }
  })
})

describe('toFiles', () => {
  // A folder's index.mdx is the folder's own link in Fumadocs; listing it in
  // the folder's pages as well draws it a second time as a child.
  test('leaves a folder index out of the folder page list', () => {
    const meta = JSON.parse(toFiles(pages).get('commands/meta.json')!)
    expect(meta.pages.length).toBeGreaterThan(25)
    expect(meta.pages).not.toContain('index')
  })
})
