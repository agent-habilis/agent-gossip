/**
 * Loading the gossip client.
 *
 * The *promise* is memoised, not the module. `__wbg_init` sets its own guard
 * only after an await, so two overlapping callers each build a
 * `WebAssembly.Instance` over one shared glue closure table — which is not a
 * wasted download but a corrupted runtime. agent-share paid for this one.
 */
import init, { GossipPeer } from './pkg/agent_gossip_wasm_client.js'

export type { GossipPeer }

let loading: Promise<typeof GossipPeer> | undefined

export function loadWasm(): Promise<typeof GossipPeer> {
  // The explicit path is required: the glue's own `import.meta.url` guess
  // resolves next to the JS chunk, and the `.wasm` is staged separately under a
  // content-addressed name.
  loading ??= init({ module_or_path: __WASM_PATH__ })
    .then(() => GossipPeer)
    .catch((error: unknown) => {
      // Cleared so a failed eager load cannot poison the real call that follows
      // it — the next caller retries rather than inheriting the rejection.
      loading = undefined
      throw error
    })
  return loading
}
