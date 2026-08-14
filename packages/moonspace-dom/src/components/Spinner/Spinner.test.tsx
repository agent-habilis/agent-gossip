import { afterEach, beforeEach, expect, test } from 'bun:test'
import { flushSync, render } from 'visage-dom'
import type { Root } from 'visage-dom'
import { asciiSpinnerFrames, spinnerFrames } from 'moonspace'
import { Spinner } from './Spinner.tsx'

let host: HTMLElement
let root: Root | null = null

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/*
 * Every test unmounts. A spinner left mounted keeps a `setInterval` alive, and
 * an unreferenced timer is enough to stop the test runner from exiting — the
 * failure mode this component is prone to, and the one the last test asserts
 * against directly.
 */
afterEach(() => {
  root?.unmount()
  root = null
})

const part = (name: string): HTMLElement => host.querySelector(`[data-part="${name}"]`)!

/** Runs `body` with the timer functions replaced, recording the ids they see. */
function withTimerSpy(body: (started: unknown[], cleared: unknown[]) => void): void {
  const realSet = globalThis.setInterval
  const realClear = globalThis.clearInterval
  const started: unknown[] = []
  const cleared: unknown[] = []

  globalThis.setInterval = ((fn: () => void, ms: number) => {
    const id = realSet(fn, ms)
    started.push(id)
    return id
  }) as typeof globalThis.setInterval
  globalThis.clearInterval = ((id: never) => {
    cleared.push(id)
    realClear(id)
  }) as typeof globalThis.clearInterval

  try {
    body(started, cleared)
  } finally {
    globalThis.setInterval = realSet
    globalThis.clearInterval = realClear
  }
}

test('announces itself as a status, with the glyph hidden and the label read', () => {
  root = render(<Spinner label="Building" />, host)
  const el = host.querySelector('span')!

  expect(el.getAttribute('role')).toBe('status')
  expect(part('frame').getAttribute('aria-hidden')).toBe('true')
  expect(part('label').textContent).toBe('Building')
  // Visually hidden rather than absent: the status has to be words, not a
  // Braille character read aloud.
  expect(el.querySelector('style')!.textContent).toContain('clip-path:inset(50%)')
})

test('starts on the first Braille frame, and ascii swaps the set', () => {
  root = render(<Spinner />, host)
  expect(part('frame').textContent).toBe(spinnerFrames[0]!)

  root.unmount()
  root = render(<Spinner ascii />, host)
  expect(part('frame').textContent).toBe(asciiSpinnerFrames[0]!)
})

test('the colour role resolves to a custom property, not a hex', () => {
  root = render(<Spinner color="warning" />, host)
  const el = host.querySelector('span')!
  expect(el.style.getPropertyValue('--ms-spinner-color')).toBe('var(--warning)')
})

test('the frame advances as the interval fires', async () => {
  root = render(<Spinner interval={5} />, host)
  const frame = part('frame')
  expect(frame.textContent).toBe(spinnerFrames[0]!)

  await Bun.sleep(40)
  flushSync()
  expect(frame.textContent).not.toBe(spinnerFrames[0]!)
})

test('unmount disposes the interval', () => {
  withTimerSpy((started, cleared) => {
    const local = render(<Spinner interval={5} />, host)
    expect(started).toHaveLength(1)

    local.unmount()
    /*
     * The whole point of `using _tick`. An undisposed timer holds the event
     * loop open, so getting this wrong does not fail a later assertion — it
     * hangs the run.
     */
    expect(cleared).toEqual(started)
  })
})

test('prefers-reduced-motion holds a single frame and starts no timer', () => {
  const realMatch = window.matchMedia
  window.matchMedia = ((query: string) => ({ matches: true, media: query })) as typeof matchMedia

  try {
    withTimerSpy((started) => {
      const local = render(<Spinner interval={5} />, host)
      // The `disposable(() => {})` arm of the ternary: nothing to clear because
      // nothing was scheduled.
      expect(started).toHaveLength(0)
      expect(part('frame').textContent).toBe(spinnerFrames[0]!)
      local.unmount()
    })
  } finally {
    window.matchMedia = realMatch
  }
})
