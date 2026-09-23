// First, before anything can build a Disposable. See the file for why.
import './compat.ts'

import { GlobalStyle, MoonspaceTheme } from 'moonspace-dom'
import { component, render, signal } from 'visage-dom'

import './app.css'

import { Connecting } from './components/Connecting/index.tsx'
import { registerAgentTools } from './lib/agentTools/index.ts'
import { boot, bootWithSplash } from './lib/boot.ts'
import { App } from './pages/index.tsx'

// Kick the bootstrap off at module scope, not on mount: the wasm fetch and
// compile then overlap the splash rather than starting after it.
boot()

// Publish the page's tools to an agent, if this browser speaks WebMCP. A no-op
// on every browser that does not, which is currently all of them by default —
// so it is not worth waiting for, and not worth failing the page over. It is
// deliberately not gated on the splash: an agent may drive a tab nobody is
// looking at, and tools that need a gossip say so themselves.
void registerAgentTools().catch((error: unknown) => {
  console.debug('[agent-gossip] publishing agent tools failed', error)
})

const Root = component(function* () {
  const ready = signal(false)

  // Never rejects, so there is no failure path to handle here — a boot that
  // failed still lets the router mount, and the room reports it in context.
  void bootWithSplash().then(() => {
    ready.value = true
  })

  yield () => (
    <>
      {MoonspaceTheme()}
      {GlobalStyle()}
      {ready.value ? <App /> : <Connecting />}
    </>
  )
})

/**
 * The page loads this as `<script type="module" async>`, which React may hoist
 * into `<head>`. An async module runs as soon as its fetch finishes and does
 * not wait for the parser, so on a warm cache it can beat `#root` into
 * existence — and throwing there leaves a blank page with no retry.
 */
function mount(): void {
  const root = document.getElementById('root')
  if (!root) throw new Error('#root is missing from the page')
  render(<Root />, root)
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', mount, { once: true })
} else {
  mount()
}
