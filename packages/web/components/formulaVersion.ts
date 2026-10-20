// The tap's formula, not the crate: it names the version `brew install` gives
// today, and it moves on release without a redeploy of this static site.
// raw.githubusercontent.com answers with `access-control-allow-origin: *`.
export const FORMULA_ORIGIN = 'https://raw.githubusercontent.com'
const FORMULA_URL = `${FORMULA_ORIGIN}/agent-habilis/homebrew-tap/main/Formula/agent-gossip.rb`

export function parseFormulaVersion(source: string): string | undefined {
  return /^\s*version\s+"([^"]+)"/m.exec(source)?.[1]
}

export async function fetchFormulaVersion(signal: AbortSignal): Promise<string | undefined> {
  const response = await fetch(FORMULA_URL, { signal })
  if (!response.ok) return undefined
  return parseFormulaVersion(await response.text())
}
