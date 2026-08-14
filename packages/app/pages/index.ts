import { createRouter, type RouteDef } from 'visage-router'

import { HomePage } from './room/index.tsx'
import { RoomLayout } from './[id]/index.tsx'

/**
 * The directory layout under `pages/` mirrors the URL: `pages/room/` is
 * `/room/`, and `pages/[id]/` is the `/<mesh-id>` room.
 *
 * The literal is ordered first, though it could not collide anyway: `room` is
 * four base58 characters, which decodes to fewer bytes than the four-byte
 * checksum needs, so it fails the id test the server applies.
 */
export const ROUTES: readonly RouteDef[] = [
  { path: '/room', component: HomePage },
  { path: '/:id', component: RoomLayout },
]

export const App = createRouter({
  routes: ROUTES,
  fallback: HomePage,
  // The app is a fixed 100dvh column, so the router's scroll restoration has
  // nothing to restore and its listener plus flushSync on every navigation is
  // pure cost.
  scroll: false,
})
