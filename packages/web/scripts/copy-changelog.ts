/**
 * Copies the repo's CHANGELOG.md into `generated/`, where the docs page imports
 * it. knope writes the root file on every release, so the docs render it
 * rather than keep a copy that could drift; Turbopack resolves nothing outside
 * the workspace, hence the copy. In the Docker image the root file arrives
 * through the `changelog` build context, at the same relative path.
 */
import { copyFile, mkdir } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const here = (path: string) => fileURLToPath(new URL(`../${path}`, import.meta.url))

await mkdir(here('generated/'), { recursive: true })
await copyFile(here('../../CHANGELOG.md'), here('generated/changelog.md'))
