import { component } from 'visage-dom'

import { openMesh, readNickname } from '../lib/route.ts'
import { HomePage } from './home/index.tsx'
import { Room } from './room/index.tsx'

/**
 * The two states of `/app/`: the front door, or a room. There is no router
 * left — see `lib/route.ts` for why the mesh is a query parameter.
 *
 * Keyed on the mesh id, and that is load-bearing rather than tidiness: without
 * the key, moving between two rooms would reuse the first room's wasm client.
 */
export const App = component(function* () {
  yield () => {
    const mesh = openMesh.value
    if (mesh === undefined) return <HomePage />
    return <Room key={mesh} mesh={mesh} nickname={readNickname()} />
  }
})
