'use client'
import { useEffect, useState } from 'react'
import { preconnect } from 'react-dom'

import { FORMULA_ORIGIN, fetchFormulaVersion } from './formulaVersion'

const TIMEOUT_MS = 3000

interface Props {
  /** The crate version at build time: shown when the formula cannot be read. */
  fallback: string
  /** Its release on the changelog page. */
  href: string
}

export function VersionChip({ fallback, href }: Props) {
  // The fetch starts only after hydration; a warm connection shortens the
  // time the chip stays blank.
  preconnect(FORMULA_ORIGIN, { crossOrigin: 'anonymous' })
  const [version, setVersion] = useState<string>()

  useEffect(() => {
    const unmount = new AbortController()
    // Inside the async body so that a browser without AbortSignal.any throws
    // into the catch and shows the fallback, not a chip that stays hidden.
    const read = async () => {
      try {
        return await fetchFormulaVersion(AbortSignal.any([unmount.signal, AbortSignal.timeout(TIMEOUT_MS)]))
      } catch {
        return undefined
      }
    }
    read().then((fetched) => {
      // A timeout reveals the fallback; an unmount (Strict Mode's rehearsal
      // included) must not, or the fetch that follows would swap the text twice.
      if (!unmount.signal.aborted) setVersion(fetched ?? fallback)
    })
    return () => unmount.abort()
  }, [fallback])

  // The formula is the truth, even when the tap lags this build: the chip names
  // what `brew install` gives today.
  const shown = version ?? fallback
  // A release newer than this build has no anchor on the deployed page yet.
  const releaseNotes = shown === fallback ? href : href.replace(/#.*/, '')
  return (
    <a className="p-button" data-variant="secondary" data-size="small" href={releaseNotes}>
      {/* The fallback holds the slot's width until the fetch settles, hidden
          rather than absent, so the chip never resizes the hero. */}
      <span className="version-slot" data-pending={version === undefined ? '' : undefined}>
        v{shown}
      </span>
    </a>
  )
}
