import { signal } from 'visage-dom'

/**
 * The whole app is one exported page, `/app/`, because the site is a static
 * export with no server that could resolve `/<mesh-id>` into a shell. So the
 * room is a query parameter rather than a path, and this module is the entire
 * router: which mesh is open, and how to change it.
 */
const PATH = '/app/'

function readMesh(): string | undefined {
  // `?mesh=` yields '', which is not undefined — without the guard a bare
  // `?mesh=` mounts a room with no id at all. A *mistyped* id still gets as far
  // as the transport, and reports there.
  return new URLSearchParams(location.search).get('mesh') || undefined
}

/** The open mesh, or `undefined` on the front door. */
export const openMesh = signal(readMesh())

/** A nickname carried in the invite link, if the sharer put one there. */
export function readNickname(): string | undefined {
  return new URLSearchParams(location.search).get('nickname') ?? undefined
}

// Back and forward have to move between the front door and a room, which means
// the signal cannot be the only source of truth for the URL.
addEventListener('popstate', () => {
  openMesh.value = readMesh()
})

/** Absolute, because the only reason to build one is to paste it elsewhere. */
export function meshUrl(mesh: string): string {
  return `${location.origin}${PATH}?mesh=${encodeURIComponent(mesh)}`
}

export function openRoom(mesh: string, options?: { replace?: boolean }): void {
  const url = `${PATH}?mesh=${encodeURIComponent(mesh)}`
  if (options?.replace) history.replaceState(null, '', url)
  else history.pushState(null, '', url)
  openMesh.value = mesh
}
