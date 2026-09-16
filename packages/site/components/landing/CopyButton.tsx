'use client'
import { useEffect, useState } from 'react'

import { CheckIcon, CopyIcon } from './icons'

/**
 * `text` is copied verbatim — never the rendered content, which carries the `$`
 * prompt and the comment lines that must not land in someone's shell.
 */
export function CopyButton({
  text,
  children,
  className,
  label = 'Copy command',
}: {
  text: string
  children?: React.ReactNode
  className?: string
  label?: string
}) {
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    if (!copied) return
    const timer = setTimeout(() => setCopied(false), 1500)
    return () => clearTimeout(timer)
  }, [copied])

  return (
    <button
      type="button"
      className={className}
      aria-label={label}
      onClick={() => {
        void navigator.clipboard.writeText(text).then(() => setCopied(true))
      }}
    >
      {children}
      {copied ? <CheckIcon /> : <CopyIcon />}
      <span className="sr-only" aria-live="polite">
        {copied ? 'Copied' : ''}
      </span>
    </button>
  )
}
