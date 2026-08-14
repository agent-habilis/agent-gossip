import { component, keyed, signal } from 'visage-dom'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css } from 'visage-style'
import { FOCUS_RING, INVERSE, ONE_ROW, rows } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export interface Tab {
  id: string
  label: string
  content?: Child
  disabled?: boolean
}

export interface TabsProps extends Attrs<'div'> {
  tabs: Tab[]
  /** Controlled selection. Omit to let the component manage it. */
  value?: string
  onValueChange?: (id: string) => void
  defaultValue?: string
}

/**
 * The bar and its triggers, in one stylesheet scoped to the bar.
 *
 * The triggers are styled as descendants rather than each carrying its own
 * `<style>`: a tab bar is a handful of buttons, but the rules are the bar's
 * rules and reading them in one block is how the selected/hover/disabled order
 * stays legible.
 */
const LIST = css({
  ...ONE_ROW,
  display: 'flex',
  gap: 0,

  '[role="tab"]': {
    ...ONE_ROW,
    // Two multi-value shorthands the value grammar does not admit — `padding: 0
    // 2ch` has to be written as the two logical properties. `paddingBlock: 0` is
    // not redundant: it undoes the user-agent button padding, which would
    // otherwise make the bar taller than one row.
    paddingInline: '2ch',
    paddingBlock: 0,
    border: 0,
    background: 'transparent',
    color: t.fgMuted,
    cursor: 'pointer',
    whiteSpace: 'nowrap',
  },

  /*
   * `aria-selected` is both the state and the selector.
   *
   * The styled-components version carried a `$selected` transient prop next to
   * the ARIA attribute and had to keep the two in step; the attribute has to be
   * correct regardless, so anything mirroring it is a second source of truth for
   * no gain. Inverse video rather than an underline or a coloured border: an
   * underline is a sub-row graphic a terminal cannot place and would push the
   * bar off a whole-row height, while inversion costs nothing and survives
   * monochrome.
   */
  '[role="tab"][aria-selected="true"]': INVERSE,

  '[role="tab"]:hover:not(:disabled):not([aria-selected="true"])': { color: t.fg },
  '[role="tab"]:focus-visible': FOCUS_RING,
  '[role="tab"]:disabled': { color: t.fgSubtle, cursor: 'not-allowed' },
})

/** One row of air between the bar and its panel, so the panel stays on the grid. */
const PANEL = css({ paddingTop: rows(1) })

/**
 * Instance counter, standing in for `useId`.
 *
 * visage has no `useId`, and does not need one. The generator prologue runs
 * exactly once per instance — that is precisely what `useState`'s initialiser
 * bought in React — so an id read here is unique per instance and stable across
 * every re-render. None of `useId`'s hydration machinery applies either: nothing
 * in this package renders on a server, so no id has to match one generated
 * somewhere else.
 */
let nextId = 0

/**
 * A tab bar.
 *
 * Arrow keys move between tabs, following the WAI-ARIA tabs pattern: only the
 * selected tab is in the tab order, so Tab reaches the bar and the arrows move
 * within it.
 */
export const Tabs = component<TabsProps>(function* (props) {
  const baseId = `ms-tabs-${nextId++}`

  /*
   * The uncontrolled half of the state. A signal rather than a plain variable
   * because a click happens outside any render — writing a signal is what wakes
   * the thunk. The controlled half needs nothing at all: `props.value` is read
   * inside the thunk, so a parent changing it re-renders for the same reason.
   */
  const internal = signal(props.defaultValue ?? props.tabs[0]?.id ?? '')

  /** The current selection, read fresh — handlers must never see a stale one. */
  const selected = (): string => props.value ?? internal.value

  const select = (id: string): void => {
    // A controlled parent owns the value. Writing `internal` as well would leave
    // two sources of truth that disagree the moment the parent declines a change.
    if (props.value === undefined) internal.value = id
    props.onValueChange?.(id)
  }

  /** Skips disabled tabs, and wraps at both ends. */
  const move = (direction: 1 | -1): void => {
    const enabled = props.tabs.filter((tab) => !tab.disabled)
    const index = enabled.findIndex((tab) => tab.id === selected())
    const next = enabled[(index + direction + enabled.length) % enabled.length]
    if (next) select(next.id)
  }

  yield () => {
    const { tabs, value, onValueChange, defaultValue, ...rest } = props
    const current = value ?? internal.value
    const active = tabs.find((tab) => tab.id === current)

    return (
      <div {...rest}>
        <div attrs={{ role: 'tablist' }}>
          {/*
           * A `<style>` inside a flex container, which is fine: the user-agent
           * sheet gives it `display: none`, so it generates no box and is not a
           * flex item. Scoping is positional, so it has to be in here.
           */}
          {Style(LIST)}
          {keyed(
            tabs,
            (tab) => tab.id,
            (tab) => (
              <button
                id={`${baseId}-tab-${tab.id}`}
                type="button"
                disabled={tab.disabled}
                // Roving tab index: one stop for the whole bar.
                tabIndex={tab.id === current ? 0 : -1}
                attrs={{
                  role: 'tab',
                  // A string, not a boolean: `attrs` removes an attribute
                  // outright on `false`, and an absent `aria-selected` reads as
                  // "not a tab" rather than "not selected".
                  'aria-selected': String(tab.id === current),
                  'aria-controls': `${baseId}-panel-${tab.id}`,
                }}
                onclick={() => select(tab.id)}
                onkeydown={(event) => {
                  // Both arrows scroll the page otherwise, and the bar is
                  // frequently the widest thing on it.
                  if (event.key === 'ArrowRight') {
                    event.preventDefault()
                    move(1)
                  } else if (event.key === 'ArrowLeft') {
                    event.preventDefault()
                    move(-1)
                  }
                }}
              >
                {tab.label}
              </button>
            ),
          )}
        </div>

        {active?.content != null && (
          <div
            id={`${baseId}-panel-${active.id}`}
            tabIndex={0}
            attrs={{ role: 'tabpanel', 'aria-labelledby': `${baseId}-tab-${active.id}` }}
          >
            {Style(PANEL)}
            {active.content}
          </div>
        )}
      </div>
    )
  }
})
