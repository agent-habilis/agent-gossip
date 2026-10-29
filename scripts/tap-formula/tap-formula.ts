export const TARGETS = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
];

const PREVIEW = /^(\d+)\.(\d+)\.(\d+)(?:-preview\.(\d+))?$/;

export interface Release {
  tag: string;
  assets: string[];
}

// Homebrew order: a preview sorts below its stable release, and a preview
// number compares as a number, so preview.10 is above preview.9.
export function compareVersions(left: string, right: string): number {
  const a = parse(left);
  const b = parse(right);
  for (const index of [0, 1, 2]) {
    if (a[index] !== b[index]) return a[index] - b[index];
  }
  const aPreview = a[3] ?? Infinity;
  const bPreview = b[3] ?? Infinity;
  return aPreview - bPreview;
}

function parse(version: string): [number, number, number, number | undefined] {
  const match = PREVIEW.exec(version);
  if (!match) throw new Error(`not a version: ${version}`);
  const [, major, minor, patch, preview] = match;
  return [Number(major), Number(minor), Number(patch), preview === undefined ? undefined : Number(preview)];
}

function isReady(release: Release, version: string): boolean {
  return TARGETS.every(
    (target) =>
      release.assets.includes(`agent-gossip-v${version}-${target}.tar.gz`) &&
      release.assets.includes(`agent-gossip-v${version}-${target}.sha256`),
  );
}

export function pickReadyPreview(releases: Release[]): Release | undefined {
  let best: Release | undefined;
  for (const release of releases) {
    const version = release.tag.slice(1);
    if (!PREVIEW.exec(version)?.[4] || !isReady(release, version)) continue;
    if (!best || compareVersions(version, best.tag.slice(1)) > 0) best = release;
  }
  return best;
}

export function rewriteFormula(formula: string, version: string, sums: Record<string, string>): string {
  let out = formula.replace(/^(\s*version )"[^"]*"/m, `$1"${version}"`);
  for (const target of TARGETS) {
    out = out.replace(
      new RegExp(`(${target}\\.tar\\.gz"\\s*\\n\\s*sha256 ")[0-9a-f]{64}`),
      `$1${sums[target]}`,
    );
  }
  return out;
}
