#!/usr/bin/env bun

export type Level = "major" | "minor" | "patch";

// While the major version is 0, levels shift down one, as knope and Cargo do.
export function nextVersion(current: string, level: Level): string {
  const [major, minor, patch] = current.split(".").map(Number);
  if (major === 0) {
    return level === "major" ? `0.${minor + 1}.0` : `0.${minor}.${patch + 1}`;
  }
  if (level === "major") return `${major + 1}.0.0`;
  if (level === "minor") return `${major}.${minor + 1}.0`;
  return `${major}.${minor}.${patch + 1}`;
}

export function previewTag(cargoVersion: string, commitCount: number): string {
  return `v${nextVersion(cargoVersion, "patch")}-preview.${commitCount}`;
}

// A tag made by an earlier run points at a throwaway commit whose parent is
// the HEAD of that run. Reuse it only when the parent is this HEAD.
export function tagAction(tag: string, tagParent: string | undefined, head: string): "create" | "reuse" {
  if (tagParent === undefined) return "create";
  if (tagParent === head) return "reuse";
  throw new Error(`${tag} exists on another commit (its parent ${tagParent} is not HEAD ${head})`);
}

async function cargoVersion(): Promise<string> {
  const manifest = await Bun.file("crates/agent-gossip/Cargo.toml").text();
  return /^version = "([^"]*)"/m.exec(manifest)![1];
}

// The count restarts at the commit that set the version, so it is unique per
// commit on main and resets on each release bump.
if (import.meta.main) {
  const [command, ...args] = process.argv.slice(2);
  if (command === "tag") {
    const setter = (await Bun.$`git log -1 --format=%H -G ${"^version = "} -- crates/agent-gossip/Cargo.toml`.text()).trim();
    const count = Number((await Bun.$`git rev-list --count ${setter}..HEAD`.text()).trim());
    console.log(previewTag(await cargoVersion(), count));
  } else if (command === "action") {
    const [tag, head] = args;
    const parent = await Bun.$`git rev-parse --verify --quiet ${`${tag}^{commit}^`}`.nothrow().quiet();
    console.log(tagAction(tag, parent.exitCode === 0 ? parent.text().trim() : undefined, head));
  }
}
