import { Spinner, Stack, Text } from 'moonspace-dom'

/**
 * The boot splash: one spinner and one word, centred in both axes.
 *
 * Deliberately not inside `Chrome` — the top bar has nothing true to say yet
 * (no mesh, no peers, no transport), and drawing it empty then filling it in
 * would move every row on the page once connecting finishes.
 */
export function Connecting({ label = 'connecting' }: { label?: string }) {
  return (
    <div
      data-status="connecting"
      style={{
        // dvh, not vh: on mobile Safari the toolbars make vh taller than the
        // visible page, which pushes a "centred" thing below the fold.
        height: '100dvh',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
      }}
    >
      {/*
        `justify="center"` is not redundant with the wrapper's own centering.
        `align="center"` makes this Stack snap its width to a whole number of
        cells, which means taking all of it — so the wrapper has no slack left
        to centre, and the main axis has to be centred here instead.
      */}
      <Stack direction="row" gap={1} align="center" justify="center">
        <Spinner label={label} />
        <Text color="fgMuted">{label}</Text>
      </Stack>
    </div>
  )
}
