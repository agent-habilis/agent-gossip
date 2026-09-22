/**
 * The one gossip this tab is in, as the tools see it.
 *
 * The room publishes itself here when it joins and withdraws on unmount, so a
 * tool never holds a client that outlived its component. Everything is reached
 * through this seam rather than through imports, which is what lets the tools
 * be written and tested before the wasm client exists.
 */

export interface RosterPeer {
  nickname: string
}

export interface GossipMessage {
  seq: number
  from: string
  text: string
  /**
   * A broadcast reaches the whole mesh. `system` is the client reporting about
   * itself — a send that failed — and is never mesh traffic.
   *
   * No `msg`: directed frames must be sealed, and the client does not send them
   * (see the wasm crate's `wire.rs`). Declaring a variant no producer can emit
   * only invites handling for a case that cannot arrive.
   */
  kind: 'broadcast' | 'system'
}

/** What a joined room offers an agent. Implemented by the wasm client. */
export interface GossipSession {
  readonly mesh: string
  readonly name: string
  readonly nickname: string
  readonly transport: 'webrtc' | 'relay'
  peers(): readonly RosterPeer[]
  broadcast(text: string): Promise<{ id?: string }>
  msg(to: string, text: string): Promise<{ id: string }>
  messages(after?: number): Promise<{ messages: GossipMessage[]; currentSeq: number }>
  ping(): Promise<readonly RosterPeer[]>
  getState(): Promise<unknown>
  mergeState(patch: Record<string, unknown>): Promise<unknown>
  getMeta(): Promise<unknown>
  mergeMeta(patch: Record<string, unknown>): Promise<unknown>
  taskStatus(taskId: string, state: string, note?: string): Promise<unknown>
  taskArtifact(taskId: string, text: string): Promise<unknown>
  a2aCall(to: string, method: string, params: unknown, timeoutSecs: number): Promise<unknown>
  leave(): Promise<void>
}

let current: GossipSession | undefined

export function publishSession(session: GossipSession): () => void {
  current = session
  return () => {
    // Only withdraw if it is still ours: a keyed remount mounts the next room
    // before unmounting the last, so an unconditional clear would erase the
    // session that just arrived.
    if (current === session) current = undefined
  }
}

export function activeSession(): GossipSession | undefined {
  return current
}

/** Test seam. Never called by the app. */
export function resetSession(): void {
  current = undefined
}
