import { Button, Input, MiddleTruncate, Stack, Text, t } from 'moonspace-dom'
import { component, disposable, interval, keyed, signal, timeout } from 'visage-dom'
import { useParams, useSearchParams } from 'visage-router'

import { Chrome } from '../../components/Chrome/index.tsx'
import { Centered } from '../../components/Centered/index.tsx'
import { Connecting } from '../../components/Connecting/index.tsx'
import { joinMesh, type Joined } from '../../lib/mesh.ts'
import type { GossipMessage, RosterPeer } from '../../lib/agentTools/session.ts'

type Phase =
  | { phase: 'connecting' }
  | { phase: 'ready' }
  | { phase: 'failed'; reason: string }

/** How often the page pulls new messages and roster out of the client. */
const POLL_MS = 500

/**
 * The room at `/<mesh-id>`.
 *
 * The splash stays up until the mesh is actually joined — not until the assets
 * finished loading. A room that painted its composer before the transport was
 * up would be a chat window that silently drops what you type.
 */
const Room = component<{ mesh: string; nickname?: string }>(function* (props) {
  const state = signal<Phase>({ phase: 'connecting' })
  const messages = signal<readonly GossipMessage[]>([])
  const peers = signal<readonly RosterPeer[]>([])
  const copied = signal(false)

  const mesh = props.mesh
  const nickname = props.nickname

  let joined: Joined | undefined
  // Not reactive: only the send handler reads it, and re-rendering on every
  // keystroke would fight the input's own cursor.
  let typed = ''
  let seen = 0
  // The last roster JSON, compared raw. Parsing it every tick to then compare
  // lengths threw away the parse and missed a same-size swap besides.
  let rosterJson = ''

  void joinMesh(mesh, { nickname }).then(
    (peer) => {
      joined = peer
      state.value = { phase: 'ready' }
    },
    (error: unknown) => {
      state.value = {
        phase: 'failed',
        reason: error instanceof Error ? error.message : String(error),
      }
    },
  )

  // One sampler for the whole room. Two would each see part of the picture, and
  // `messages(after)` is a cursor — a second reader would advance it past the
  // first.
  using _poll = interval(POLL_MS, () => {
    if (!joined) return
    const fresh = joined.messages(seen)
    if (fresh.length > 0) {
      seen = fresh[fresh.length - 1]?.seq ?? seen
      messages.value = [...messages.value, ...fresh]
    }
    const fresh_roster = joined.rosterJson()
    if (fresh_roster !== rosterJson) {
      rosterJson = fresh_roster
      peers.value = joined.peers()
    }
  })

  using _leave = disposable(() => {
    void joined?.leave()
  })

  function send() {
    const text = typed.trim()
    if (!text || !joined) return
    typed = ''
    const field = document.querySelector<HTMLInputElement>('[data-field=composer]')
    if (field) field.value = ''
    void joined.broadcast(text).catch((error: unknown) => {
      // The text is already out of the composer, so silence here loses it. The
      // Rust driver reports its own send failures the same way.
      messages.value = [
        ...messages.value,
        {
          seq: (messages.value.at(-1)?.seq ?? 0) + 1,
          from: 'system',
          kind: 'system',
          text: `could not send: ${error instanceof Error ? error.message : String(error)}`,
        },
      ]
    })
  }

  let resetCopied: Disposable | undefined
  using _resetCopied = disposable(() => resetCopied?.[Symbol.dispose]())

  async function copyLink() {
    const url = `${location.origin}/${mesh}`
    try {
      await navigator.clipboard.writeText(url)
      copied.value = true
      // `timeout`, not setTimeout: leaving the room inside 2s would otherwise
      // fire this against an unmounted component.
      resetCopied?.[Symbol.dispose]()
      resetCopied = timeout(2000, () => (copied.value = false))
    } catch {
      // Refused without a gesture, or no permission. Selecting the text is the
      // fallback that always works.
      document.querySelector<HTMLElement>('[data-invite-url]')?.focus()
    }
  }

  yield () => {
    const current = state.value

    if (current.phase === 'connecting') return <Connecting />

    if (current.phase === 'failed') {
      return (
        <Chrome crumb={<Text color="fgMuted"><MiddleTruncate value={mesh} /></Text>}>
          <Centered>
            <Stack direction="column" gap={1} align="center" justify="center">
              <Text color="danger" data-status="failed">
                could not join this gossip
              </Text>
              <Text color="fgSubtle">{current.reason}</Text>
            </Stack>
          </Centered>
        </Chrome>
      )
    }

    const roster = peers.value
    const log = messages.value

    return (
      <Chrome
        crumb={
          <Text color="fgMuted" data-mesh={mesh}>
            <MiddleTruncate value={mesh} />
          </Text>
        }
        actions={
          <>
            <Text color="fgSubtle" data-status="ready" data-peers={String(roster.length)}>
              {roster.length === 0 ? 'alone' : `${roster.length} peer${roster.length === 1 ? '' : 's'}`}
            </Text>
            <Button variant="primary" data-action="copy" onclick={() => void copyLink()}>
              {copied.value ? 'copied' : 'copy'}
            </Button>
          </>
        }
      >
        {/* The transcript scrolls; the bar above and composer below do not. */}
        <div
          data-transcript=""
          style={{ flex: 1, minHeight: 0, overflowY: 'auto', padding: '1ch 2ch' }}
        >
          {roster.length === 0 && log.length === 0 ? <Invite mesh={mesh} /> : null}
          {keyed(
            log,
            (message) => message.seq,
            (message) => (
              <div data-kind={message.kind} data-text={message.text} data-from={message.from}>
                <Text color={message.kind === 'system' ? 'danger' : 'fg'}>
                  <Text color="fgSubtle">{message.from} </Text>
                  {message.text}
                </Text>
              </div>
            ),
          )}
        </div>

        <div style={{ flexShrink: 0, padding: '1ch 2ch', borderTop: `1px solid ${t.border}` }}>
          <Input
            autofocus
            data-field="composer"
            placeholder="say something"
            oninput={(event: Event) => {
              typed = (event.target as HTMLInputElement).value
            }}
            onkeydown={(event: KeyboardEvent) => {
              if (event.key === 'Enter') send()
            }}
          />
        </div>
      </Chrome>
    )
  }
})

/**
 * A fresh room has exactly one peer in it, and a blank transcript says nothing
 * about what to do next. The link is absolute because the point is pasting it
 * somewhere else.
 */
function Invite({ mesh }: { mesh: string }) {
  return (
    <Stack direction="column" gap={1} data-invite="">
      <Text color="fgMuted">nobody else is here yet. share this link:</Text>
      <Text class="selectable" data-invite-url="">
        {`${location.origin}/${mesh}`}
      </Text>
      <Text color="fgMuted">or, from a terminal:</Text>
      <Text class="selectable" color="fgSubtle">{`agent-gossip join ${mesh}`}</Text>
    </Stack>
  )
}

/**
 * Keyed on the mesh id, and that is load-bearing rather than tidiness: the
 * router keeps a depth-0 route component alive across navigations, so without
 * the key a move between two rooms would reuse the first room's client.
 */
export const RoomLayout = component(function* () {
  const params = useParams(this)
  const [search] = useSearchParams(this)

  yield () => {
    const mesh = params.value['id'] ?? ''
    const nickname = search.value.get('nickname') ?? undefined
    return <Room key={mesh} mesh={mesh} nickname={nickname} />
  }
})
