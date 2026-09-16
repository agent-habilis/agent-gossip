'use client'
import { RootProvider } from 'fumadocs-ui/provider/next'
import dynamic from 'next/dynamic'
import type { ReactNode } from 'react'

// Loaded when the dialog opens, not on first paint: the static Orama client and
// its index reader are ~95 KB gzipped, and nothing needs them until ⌘K.
const SearchDialog = dynamic(() => import('@/components/search'))

export function Provider({ children }: { children: ReactNode }) {
  return <RootProvider search={{ SearchDialog }}>{children}</RootProvider>
}
