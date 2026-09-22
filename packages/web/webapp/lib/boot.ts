/**
 * App bootstrap: everything that has to be ready before a room can render.
 *
 * Started at module load rather than when a component mounts, so the wasm fetch
 * and compile overlap the splash instead of following it.
 */
import { loadWasm } from '../wasm/index.ts'

/**
 * The splash stays up this long even when everything was already cached.
 *
 * A deliberate floor, not a guess at load time. Connecting to a gossip is not
 * instant on a cold network, so a splash that flashes for 80ms on a warm load
 * and sits for four seconds on a cold one reads as breakage; holding it makes
 * the two look the same.
 */
export const MIN_SPLASH_MS = 5_000

/**
 * Warm the client. Deliberately *not* memoised here — `loadWasm` already
 * memoises its promise and clears it on failure so a transient error can be
 * retried. A second cache over it would swallow that retry and make one failed
 * fetch poison the tab for good.
 */
export function boot(): void {
  void loadWasm().catch(() => undefined)
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/**
 * Both the work and the floor — whichever finishes last. Never rejects: a boot
 * failure is the room's problem to report, not a reason to hang the splash
 * forever.
 */
export async function bootWithSplash(): Promise<void> {
  await Promise.all([loadWasm().catch(() => undefined), delay(MIN_SPLASH_MS)])
}
