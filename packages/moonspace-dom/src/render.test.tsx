import { beforeEach, expect, test } from 'bun:test'
import { component, flushSync, render, signal } from 'visage-dom'
import { Style, css } from 'visage-style'
import { ONE_ROW } from './mixins.ts'
import { t } from './tokens.ts'

/*
 * The end-to-end smoke test for the whole foundation: JSX compiled through
 * visage's runtime, a scoped <style> carried as a child node, tokens resolving
 * to custom properties, and a signal driving an update.
 *
 * If this file passes, the plumbing works and the remaining work is only
 * translating components.
 */

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

const PROBE = css({
  ...ONE_ROW,
  display: 'inline-flex',
  color: t.accent,
  '&[data-on="true"]': { ...ONE_ROW, color: t.danger },
})

const Probe = component<{ label: string }>(function* (props) {
  const on = signal(false)
  yield () => (
    <button type="button" dataset={{ on: String(on.value) }} onclick={() => (on.value = !on.value)}>
      {Style(PROBE)}
      <span>{props.label}</span>
    </button>
  )
})

test('JSX compiles through visage, not React', () => {
  render(<Probe label="deploy" />, host)
  const button = host.querySelector('button')
  expect(button).not.toBeNull()

  /*
   * The label is read off the <span>, not off the button.
   *
   * `textContent` concatenates every descendant text node, and the scoped
   * `<style>` is a descendant — so `button.textContent` is the whole compiled
   * stylesheet followed by "deploy". That is true of every component in this
   * package now, and it is the one assertion habit that has to change from the
   * styled-components era, where the CSS lived in a sheet outside the tree.
   */
  expect(button!.querySelector('span')!.textContent).toBe('deploy')
})

test('the scoped style is a child node of the component root', () => {
  render(<Probe label="deploy" />, host)
  const button = host.querySelector('button')!
  const style = button.firstElementChild as HTMLStyleElement

  // Position is the mechanism, not decoration: @scope binds to the <style>
  // element's parent, so being inside the button is what scopes it to the button.
  expect(style.tagName).toBe('STYLE')
  expect(style.parentElement).toBe(button)
  expect(style.textContent).toContain('color:var(--accent);')
  // `@scope`, but deliberately not `@layer moonspace`: happy-dom reports no
  // layer support, so the compiler drops the wrapper here while Chrome keeps it.
  // Asserting the layer would pin the test environment rather than the CSS.
  expect(style.textContent).toContain('@scope{')
})

test('a signal write re-renders the yielded thunk', () => {
  render(<Probe label="deploy" />, host)
  const button = host.querySelector('button')!
  expect(button.dataset['on']).toBe('false')

  button.click()
  flushSync()
  expect(button.dataset['on']).toBe('true')
})

test('two instances share one compiled stylesheet', () => {
  render([Probe({ label: 'a' }), Probe({ label: 'b' })], host)
  const texts = [...host.querySelectorAll('style')].map((s) => s.textContent)

  // Hoisting with css() means the compile cache hits on object identity. Two
  // distinct texts here would mean the object was being rebuilt per instance,
  // which is also what the DEV "recompiled an identical style object" warning
  // is for.
  expect(texts).toHaveLength(2)
  expect(new Set(texts).size).toBe(1)
})

test('unmount removes everything, style element included', () => {
  const root = render(<Probe label="deploy" />, host)
  expect(host.querySelector('style')).not.toBeNull()

  root.unmount()
  // No registry and no sheet GC: the <style> is a child node, so it leaves with
  // its component. Comment holes are visage's position markers, not leaks.
  expect(host.innerHTML.replace(/<!---->/g, '')).toBe('')
})
