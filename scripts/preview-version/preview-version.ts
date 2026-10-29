#!/usr/bin/env bun

export function nextPatch(cargoVersion: string): string {
  const [major, minor, patch] = cargoVersion.split(".").map(Number);
  return `${major}.${minor}.${patch + 1}`;
}

export function previewTag(cargoVersion: string, commitCount: number): string {
  return `v${nextPatch(cargoVersion)}-preview.${commitCount}`;
}

if (import.meta.main) {
  const [cargoVersion, stableTag] = process.argv.slice(2);
  const count = await Bun.$`git rev-list --count ${stableTag}..HEAD`.text();
  console.log(previewTag(cargoVersion, Number(count.trim())));
}
