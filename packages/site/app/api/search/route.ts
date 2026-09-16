import { createFromSource } from 'fumadocs-core/search/server'

import { source } from '@/lib/source'

// Written out as a file at build time: the static export has no server, and
// the search dialog downloads this index and searches it in the browser.
export const revalidate = false

export const { staticGET: GET } = createFromSource(source, { language: 'english' })
