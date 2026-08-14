import { Button, Input, Stack, Text } from 'moonspace-dom'
import { useNavigate } from 'visage-router'
import { component, signal } from 'visage-dom'

import { Chrome } from '../../components/Chrome/index.tsx'
import { createMesh } from '../../lib/mesh.ts'
import { parseMeshInput } from '../../lib/meshId.ts'

type Mode =
  | { phase: 'choosing' }
  | { phase: 'joining'; error?: string }
  | { phase: 'creating' }
  | { phase: 'failed'; reason: string }

/**
 * The front door at `/room/`: create a gossip, or join one by id. Both land on
 * the same `/<id>` URL — there is no creator-flavoured variant of it.
 */
export const HomePage = component(function* () {
  const navigate = useNavigate(this)
  const mode = signal<Mode>({ phase: 'choosing' })
  // Not reactive: only the submit handler reads it, and re-rendering on every
  // keystroke would fight the input's own cursor.
  let typed = ''

  async function join() {
    const id = await parseMeshInput(typed)
    if (!id) {
      mode.value = { phase: 'joining', error: 'that does not look like a gossip id' }
      return
    }
    navigate(`/${id}`)
  }

  async function create() {
    mode.value = { phase: 'creating' }
    try {
      const joined = await createMesh()
      // `replace`, not push, so back returns to this page rather than re-running
      // creation and making a second mesh.
      navigate(`/${joined.mesh}`, { replace: true })
    } catch (error) {
      // Anything that goes wrong has to land somewhere visible. Leaving this
      // unhandled is what left the page sitting on "creating…" forever.
      mode.value = {
        phase: 'failed',
        reason: error instanceof Error ? error.message : String(error),
      }
    }
  }

  yield () => {
    const current = mode.value

    return (
      <Chrome>
        <div
          style={{
            flex: 1,
            minHeight: 0,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            padding: '2ch',
          }}
        >
          <Stack direction="column" gap={2} align="stretch">
            {current.phase === 'choosing' ? (
              <>
                <Button variant="primary" data-action="create" onclick={() => void create()}>
                  create a gossip
                </Button>
                <Button
                  data-action="join"
                  onclick={() => {
                    mode.value = { phase: 'joining' }
                  }}
                >
                  join a gossip
                </Button>
              </>
            ) : null}

            {current.phase === 'creating' ? (
              <Text color="fgMuted" data-status="creating">
                creating…
              </Text>
            ) : null}

            {current.phase === 'failed' ? (
              <>
                <Text color="danger" data-status="failed">
                  could not create a gossip
                </Text>
                <Text color="fgSubtle">{current.reason}</Text>
                <Button
                  data-action="back"
                  onclick={() => {
                    mode.value = { phase: 'choosing' }
                  }}
                >
                  back
                </Button>
              </>
            ) : null}

            {current.phase === 'joining' ? (
              <>
                <Text color="fgMuted">paste a gossip id or link</Text>
                <Input
                  autofocus
                  data-field="mesh"
                  placeholder="2UXAThUkdBAb…"
                  oninput={(event: Event) => {
                    typed = (event.target as HTMLInputElement).value
                  }}
                  onkeydown={(event: KeyboardEvent) => {
                    if (event.key === 'Enter') void join()
                  }}
                />
                <Button variant="primary" data-action="join-submit" onclick={() => void join()}>
                  join
                </Button>
                {current.error ? (
                  <Text color="danger" data-error="join">
                    {current.error}
                  </Text>
                ) : null}
              </>
            ) : null}
          </Stack>
        </div>
      </Chrome>
    )
  }
})
