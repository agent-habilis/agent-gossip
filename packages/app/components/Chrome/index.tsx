import { Stack, Text } from 'moonspace-dom'
import type { Child } from 'visage-dom'

import { AgentBadge } from '../AgentBadge/index.tsx'

/**
 * App chrome: a one-row top bar on the sunken page background, with the content
 * surface below it on `bg`. The same split — and the same
 * `product / view` + right-aligned-actions bar — as agent-share.
 */
export function Chrome({
  crumb,
  actions,
  children,
}: {
  /** The view. Omitted on the front door, where the brand stands alone. */
  crumb?: Child
  actions?: Child
  children: Child
}) {
  return (
    <div
      style={{
        display: 'flex',
        flexDirection: 'column',
        height: '100dvh',
        minHeight: 0,
      }}
    >
      {/*
        One row, and only ever one row: the transcript below must not shift
        when the actions change width. `flexShrink: 0` keeps the bar out of the
        scroll, and `minmax(0, …)` on the leading rail lets the crumb be the
        thing that gives on a narrow window rather than the actions overflowing
        into it.
      */}
      <div
        style={{
          flexShrink: 0,
          padding: 'var(--ms-row) 2ch',
          display: 'grid',
          gridTemplateColumns: 'minmax(0, 1fr) auto',
          alignItems: 'center',
          gap: '2ch',
        }}
      >
        <Stack direction="row" gap={1}>
          <Text weight="bold">agent-gossip</Text>
          {crumb ? (
            <>
              <Text color="fgSubtle">/</Text>
              {crumb}
            </>
          ) : null}
        </Stack>
        {/*
          The badge sits with the actions on every page, not just a room: an
          agent can drive the front door too, and a tab that showed no sign of
          it there would be the one place the evidence went missing.
        */}
        <Stack direction="row" gap={2} align="center" justify="end">
          <AgentBadge />
          {actions}
        </Stack>
      </div>

      <div
        style={{
          flex: 1,
          minHeight: 0,
          display: 'flex',
          flexDirection: 'column',
          background: 'var(--bg)',
        }}
      >
        {children}
      </div>
    </div>
  )
}
