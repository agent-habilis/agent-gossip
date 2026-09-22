import type { Child } from 'visage-dom'

/** Fills the remaining column and centres its child in both axes. */
export function Centered({ children }: { children: Child }) {
  return (
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
      {children}
    </div>
  )
}
