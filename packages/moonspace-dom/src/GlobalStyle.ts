import { createElement } from 'visage-dom/element'
import type { ElementNode } from 'visage-dom/types'
import { fontFamily, fontFeatureSettings, fontWeight } from 'moonspace'

/**
 * Global reset and grid setup.
 *
 * A plain `<style>` rather than `Style()`, and the distinction is the whole
 * reason this file exists. `Style()` emits a prelude-less `@scope`, which scopes
 * its rules to the element's parent — exactly wrong for a reset, which has to
 * reach `html`, `body`, `*` and `::selection`. `visage-style` makes the same
 * exception for `Theme()`, calling it "the one place this library emits an
 * unscoped rule"; this is the second, for the same reason.
 *
 * Being an ordinary child node also removes a bug the old `createGlobalStyle`
 * had. That was a runtime injection which unmounted and remounted as the app
 * re-rendered, leaving a frame with nothing painting the page — which is why
 * stories-dom's index.html carries a static background block. A child node
 * mounts once and is removed only with its parent.
 *
 * The body is a template string, not a style object, so the value grammar never
 * applies and `-webkit-font-smoothing`, `font-kerning`, `font-variant-ligatures`,
 * `text-size-adjust` and `scrollbar-color` pass through untouched. The custom
 * properties themselves are set by `MoonspaceTheme()`, not here.
 */

/*
 * The layer order is declared, not left to chance.
 *
 * `visage-style` puts component rules in a layer, and unlayered author CSS beats
 * every layered rule no matter where it appears — that is the "politeness" that
 * lets a library be overridden. Left alone it would invert what
 * styled-components did, and `button, input, select, textarea { font: inherit }`
 * below would outrank a component that wanted its own. Declaring `reset` first
 * and putting the reset inside it restores the intuitive order at zero cost.
 */
const CSS = `
@layer reset, moonspace;

/*
 * The light/dark switch, in three declarations — and deliberately OUTSIDE every
 * layer, unlike the reset below.
 *
 * Every colour token is a light-dark() pair (see tokens.ts), so this is the only
 * thing that decides which half is used. "light dark" means follow the OS; the
 * two attribute rules let an app force one.
 *
 * Unlayered because index.html sets color-scheme on html to paint the first
 * frame before any JavaScript runs, and unlayered author CSS beats every layered
 * rule no matter where it appears — that is the politeness that lets this library
 * be overridden, and inside @layer reset these three lost to it silently. The
 * page kept its dark values with data-theme="light" set and nothing to show why.
 *
 * Unlayered, the cascade resolves properly: :root and [data-theme] are (0,1,0)
 * against html's (0,0,1), so both beat the static block, and the attribute rules
 * beat :root on source order — they are the same element, since data-theme lands
 * on html, which *is* :root.
 *
 * color-scheme rather than a prefers-color-scheme media query because it does a
 * second job nothing else can: it also switches form controls, scrollbars and the
 * UA's own defaults. A media query would have flipped our custom properties and
 * left the browser's own rendering on the other appearance.
 */
:root { color-scheme: light dark; }
[data-theme="light"] { color-scheme: light; }
[data-theme="dark"] { color-scheme: dark; }

@layer reset {
  /*
   * TX-02 (Berkeley Mono) is proprietary and cannot be redistributed, so no font
   * files ship with this package. If you own a license, drop the .woff2 files in
   * and uncomment this block — everything else already names the family.
   *
   * @font-face {
   *   font-family: 'TX-02';
   *   src: url('/fonts/TX-02-Variable.woff2') format('woff2-variations');
   *   font-weight: 100 900;
   *   font-stretch: 60% 100%;
   *   font-style: oblique -16deg 0deg;
   *   font-display: swap;
   * }
   */

  *, *::before, *::after {
    box-sizing: border-box;
  }

  html {
    font-size: var(--ms-font-size);
    /*
     * Pin the grid to CSS pixels. Without this, a user's browser zoom or default
     * font size would rescale the cell and every ch measurement with it — fine
     * for a normal site, but here it desynchronises components that were sized
     * against a fixed row height.
     */
    -webkit-text-size-adjust: 100%;
    text-size-adjust: 100%;
  }

  body {
    margin: 0;
    background: var(--bg);
    color: var(--fg);
    font-family: ${fontFamily};
    font-size: var(--ms-font-size);
    font-weight: ${fontWeight.regular};
    line-height: var(--ms-row);
    font-feature-settings: ${fontFeatureSettings};
    /*
     * Ligatures are enabled for TX-02's contextual set but kerning is off: any
     * kerning at all would make advance widths non-uniform and break the ch unit.
     */
    font-kerning: none;
    font-variant-ligatures: contextual;
    -webkit-font-smoothing: antialiased;
  }

  /* Controls inherit the one font and the one size. There are no exceptions. */
  button, input, select, textarea {
    font: inherit;
    font-feature-settings: inherit;
    color: inherit;
  }

  ::selection {
    background: var(--bg-selected);
    color: var(--fg);
  }

  /* Scrollbars, rendered as narrow as the platform allows so they cost ~1 cell. */
  * {
    scrollbar-width: thin;
    scrollbar-color: var(--border) transparent;
  }
}
`

export function GlobalStyle(): ElementNode<'style'> {
  return createElement('style', {}, [CSS])
}
