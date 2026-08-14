/**
 * The seam between the UI and the gossip client.
 *
 * Everything above this file is written against `Joined`, so the room does not
 * change shape as the client grows.
 */
import { ToolInputError } from './agentTools/result.ts'
import {
  publishSession,
  type GossipMessage,
  type GossipSession,
  type RosterPeer,
} from './agentTools/session.ts'
import { loadWasm, type GossipPeer } from '../wasm/index.ts'

export interface Joined extends Pick<GossipSession, 'mesh' | 'name' | 'nickname' | 'transport'> {
  /** The raw JSON the client hands back, so a caller can skip an unchanged parse. */
  rosterJson(): string
  peers(): readonly RosterPeer[]
  messages(after: number): readonly GossipMessage[]
  broadcast(text: string): Promise<void>
  /** Withdraws the WebMCP session and frees the wasm peer. */
  leave(): Promise<void>
}

export interface JoinOptions {
  nickname?: string | undefined
}

function wrap(peer: GossipPeer): Joined {
  // Held so `leave` can withdraw the session and free the wasm peer. Without
  // this the module-level `current` in session.ts pins the peer — and its
  // never-trimmed message buffer — in linear memory for the tab's whole life,
  // with the agent tools still aimed at a mesh the tab already left.
  let withdraw: (() => void) | undefined

  const joined: Joined = {
    mesh: peer.meshId,
    name: peer.name,
    nickname: peer.nickname,
    // What this tab asked for. The engine settles per connection and can fall
    // back to the relay; reporting the live path needs the client to expose it.
    transport: 'webrtc',
    rosterJson: () => peer.peers,
    peers: () => JSON.parse(peer.peers) as RosterPeer[],
    messages: (after) => JSON.parse(peer.messages(after)) as GossipMessage[],
    broadcast: async (text) => {
      await peer.broadcast(text)
    },
    leave: async () => {
      withdraw?.()
      await peer.leave()
      peer.free()
    },
  }

  /**
   * Reported as `unsupported`, not as a generic failure.
   *
   * `guard` only preserves a code for `ToolInputError`, so a plain `throw` here
   * would reach the agent as `failed` — the same code as "transport closed".
   * An agent is right to retry that, and would retry forever against something
   * structurally impossible.
   */
  const unsupported = (what: string) => (): never => {
    throw new ToolInputError('unsupported', `${what} is not supported by the browser client yet`)
  }

  const session: GossipSession = {
    mesh: joined.mesh,
    name: joined.name,
    nickname: joined.nickname,
    transport: joined.transport,
    peers: () => joined.peers(),
    broadcast: async (text: string) => {
      await joined.broadcast(text)
      // No id: the client does not surface the frame id it minted, and a
      // fabricated one is worse than an absent one — an agent correlating it
      // against `fetch_messages` would match on the empty string.
      return {}
    },
    messages: async (after?: number) => {
      const messages = joined.messages(after ?? 0)
      return { messages: [...messages], currentSeq: messages.at(-1)?.seq ?? (after ?? 0) }
    },
    leave: () => joined.leave(),
    // A directed msg must arrive sealed to its addressee; see the wasm crate's
    // `wire.rs` for why the tag is deliberately absent rather than plaintext.
    msg: unsupported('sending a direct msg'),
    ping: unsupported('ping'),
    getState: unsupported('the state document'),
    mergeState: unsupported('the state document'),
    getMeta: unsupported('the meta document'),
    mergeMeta: unsupported('the meta document'),
    taskStatus: unsupported('tasks'),
    taskArtifact: unsupported('tasks'),
    a2aCall: unsupported('a2a calls'),
  }
  // Publish to the WebMCP tools, so an agent driving this tab acts on the same
  // membership the person is looking at rather than one of its own.
  withdraw = publishSession(session)

  return joined
}

/**
 * Resolves once this peer is actually on the mesh — not once the assets
 * finished loading. The room holds its splash on this promise, so resolving
 * early would paint a composer that silently drops what is typed into it.
 */
export async function joinMesh(mesh: string, options: JoinOptions = {}): Promise<Joined> {
  if (!mesh) throw new Error('no gossip id in the url')
  const Peer = await loadWasm()
  return wrap(await Peer.join(mesh, options.nickname, undefined))
}

/**
 * Create a gossip and join it. The returned `mesh` is what goes in the URL —
 * the same URL anyone else uses to join.
 */
export async function createMesh(options: JoinOptions = {}): Promise<Joined> {
  const Peer = await loadWasm()
  return wrap(await Peer.create(options.nickname, undefined))
}
