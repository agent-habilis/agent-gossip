import { expect, test } from "bun:test";
import { compareVersions, pickReadyPreview, rewriteFormula } from "./tap-formula";

const TARGETS = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
];

function release(version: string, { tarballs = TARGETS, sums = TARGETS } = {}) {
  return {
    tag: `v${version}`,
    assets: [
      ...tarballs.map((target) => `agent-gossip-v${version}-${target}.tar.gz`),
      ...sums.map((target) => `agent-gossip-v${version}-${target}.sha256`),
    ],
  };
}

test("a preview with a higher number orders later, not as text", () => {
  expect(compareVersions("0.12.11-preview.10", "0.12.11-preview.9")).toBeGreaterThan(0);
});

test("a stable release orders after every preview of the same version", () => {
  expect(compareVersions("0.12.11", "0.12.11-preview.10")).toBeGreaterThan(0);
  expect(compareVersions("0.12.12-preview.1", "0.12.11")).toBeGreaterThan(0);
});

test("the highest preview with all assets is picked", () => {
  const releases = [release("0.12.11-preview.9"), release("0.12.11-preview.10")];
  expect(pickReadyPreview(releases)?.tag).toBe("v0.12.11-preview.10");
});

test("a preview missing one tarball is skipped", () => {
  const releases = [
    release("0.12.11-preview.9"),
    release("0.12.11-preview.10", { tarballs: TARGETS.slice(1) }),
  ];
  expect(pickReadyPreview(releases)?.tag).toBe("v0.12.11-preview.9");
});

test("a preview missing one checksum is skipped", () => {
  const releases = [
    release("0.12.11-preview.9"),
    release("0.12.11-preview.10", { sums: TARGETS.slice(1) }),
  ];
  expect(pickReadyPreview(releases)?.tag).toBe("v0.12.11-preview.9");
});

test("stable tags are never picked as a preview", () => {
  expect(pickReadyPreview([release("0.12.10")])).toBeUndefined();
});

const FORMULA = `class AgentGossipPreview < Formula
  version "0.12.11-preview.9"

  on_macos do
    on_arm do
      url "https://example.test/agent-gossip-v0.12.11-preview.9-aarch64-apple-darwin.tar.gz"
      sha256 "${"a".repeat(64)}"
    end
    on_intel do
      url "https://example.test/agent-gossip-v0.12.11-preview.9-x86_64-apple-darwin.tar.gz"
      sha256 "${"a".repeat(64)}"
    end
  end

  on_linux do
    on_intel do
      url "https://example.test/agent-gossip-v0.12.11-preview.9-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "${"b".repeat(64)}"
    end
    on_arm do
      url "https://example.test/agent-gossip-v0.12.11-preview.9-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "${"b".repeat(64)}"
    end
  end
end
`;

test("the formula takes the new version and the checksum of each target", () => {
  const sums = Object.fromEntries(TARGETS.map((target) => [target, "c".repeat(64)]));
  const out = rewriteFormula(FORMULA, "0.12.11-preview.10", sums);
  expect(out).toContain('version "0.12.11-preview.10"');
  expect(out.match(new RegExp(`sha256 "${"c".repeat(64)}"`, "g"))).toHaveLength(TARGETS.length);
  expect(out).not.toContain("a".repeat(64));
});

test("the url of each target takes the new version too", () => {
  const sums = Object.fromEntries(TARGETS.map((target) => [target, "c".repeat(64)]));
  const out = rewriteFormula(FORMULA, "0.12.11-preview.10", sums);
  expect(out).toContain("agent-gossip-v0.12.11-preview.10-aarch64-apple-darwin.tar.gz");
  expect(out).toContain("agent-gossip-v0.12.11-preview.10-x86_64-unknown-linux-gnu.tar.gz");
  expect(out).not.toContain("preview.9");
});

test("a formula without the url of a target fails instead of shipping a stale url", () => {
  const sums = Object.fromEntries(TARGETS.map((target) => [target, "c".repeat(64)]));
  const withoutIntelMac = FORMULA.replace(/\s*url ".*x86_64-apple-darwin.*\n/, "\n");
  expect(() => rewriteFormula(withoutIntelMac, "0.12.11-preview.10", sums)).toThrow(/x86_64-apple-darwin/);
});
