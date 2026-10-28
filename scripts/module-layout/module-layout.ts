#!/usr/bin/env bun
// Module layout (issue #29, O9). A leaf source file is named after its primary public type: the
// file `frame_cache.rs` holds `FrameCache`. A leaf file with public types, none of which is named
// after the file, is a hit. A hit needs a counted row in `module-layout.allow` with its reason. A
// leaf with no public type is fine. `module-layout.test.ts` plants one file of each kind.

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const allowFile = process.env.MODULE_LAYOUT_ALLOW || "scripts/module-layout/module-layout.allow";

const SCOPE = /\.rs$/;
const SKIP = /\/(tests|benches|examples)\/|\/build\.rs$/;
const ROOT_FILE = /(^|\/)(mod|lib|main)\.rs$/;
const CHILD_MOD = /^\s*(pub(\([a-z]+\))? )?mod \w+;/m;
const PUB_TYPE = /^pub(?:\([a-z]+\))? (?:struct|enum|trait|union|type) (\w+)/gm;

// The Rust files git knows, minus the tests, benches, examples and build scripts.
function listFiles(): string[] {
  const proc = spawnSync("git", ["ls-files", "-z", "-co", "--exclude-standard"], {
    cwd: ROOT,
    encoding: "utf8",
  });
  return (proc.stdout ?? "").split("\0").filter((file) => SCOPE.test(file) && !SKIP.test(file));
}

// `frame_cache` for `FrameCache`: the snake_case of a type name, so `HTTPClient` names `http_client`.
function snake(camel: string): string {
  return camel
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2")
    .toLowerCase();
}

// The public types of a leaf file, read before its `#[cfg(test)]` module. A type whose snake_case
// name is the file name is a match, so the file is fine.
function leafHit(file: string, src: string): string | undefined {
  const base = path.basename(file, ".rs");
  if (ROOT_FILE.test(file) || CHILD_MOD.test(src)) return undefined;
  const body = src.split(/^#\[cfg\(test\)\]/m)[0] ?? "";
  const types = [...body.matchAll(PUB_TYPE)].map((m) => m[1] ?? "");
  if (types.length === 0) return undefined;
  if (types.some((name) => snake(name) === base)) return undefined;
  return `leaf file named for none of its public types (${types.join(", ")}). Rename it`;
}

// A mod.rs or lib.rs holds module declarations and re-exports. Up to 20 lines of definitions are
// small core glue (O13). More is a hit (O11): the items move into leaf files.
const DEFINITION = /^(pub(\([^)]*\))? )?(unsafe )?(struct|enum|trait|union|fn|impl|const|static|type)\b/m;
const GLUE_LINES = 20;
function nonLeafHit(file: string, src: string): string | undefined {
  if (!/(^|\/)(mod|lib)\.rs$/.test(file)) return undefined;
  const body = src.split(/^#\[cfg\(test\)\]/m)[0] ?? "";
  if (!DEFINITION.test(body)) return undefined;
  const lines = body.split("\n").filter((line) => line.trim() !== "" && !/^\s*(\/\/|(pub(\([^)]*\))? )?(mod|use) )/.test(line));
  return lines.length > GLUE_LINES ? "mod.rs or lib.rs defines items. Move them into a leaf file" : undefined;
}

function readAllow(): { allowed: Map<string, number>; problems: string[] } {
  const allowed = new Map<string, number>();
  const problems: string[] = [];
  const file = path.join(ROOT, allowFile);
  if (!existsSync(file)) return { allowed, problems };
  for (const line of readFileSync(file, "utf8").split("\n")) {
    if (/^\s*(#|$)/.test(line)) continue;
    const [target = "", count = "", reason = ""] = line.split("|");
    if (reason.trim() === "") {
      problems.push(`module-layout: ${allowFile} has a row for ${target} with no reason.`);
    }
    if (allowed.has(target)) {
      problems.push(`module-layout: ${allowFile} has two rows for ${target}. Merge them.`);
    }
    allowed.set(target, Number(count));
  }
  return { allowed, problems };
}

async function main(): Promise<number> {
  const { allowed, problems } = readAllow();
  const found = new Map<string, string>();
  for (const file of listFiles()) {
    const src = readFileSync(path.join(ROOT, file), "utf8");
    const hit = leafHit(file, src) ?? nonLeafHit(file, src);
    if (hit !== undefined) found.set(file, hit);
  }
  for (const [file, reason] of [...found].sort()) {
    const expected = allowed.get(file);
    if (expected === undefined) {
      problems.push(`module-layout: ${file}: ${reason}. Or add a row to ${allowFile} with the reason.`);
    } else if (expected !== 1) {
      problems.push(`module-layout: ${file}: 1 hit found, ${expected} allowed. Set the count in ${allowFile}.`);
    }
  }
  for (const file of [...allowed.keys()].sort()) {
    if (!found.has(file)) {
      problems.push(`module-layout: ${file}: stale row. The file is fine now. Remove it from ${allowFile}.`);
    }
  }
  for (const problem of problems) console.log(problem);
  return problems.length > 0 ? 1 : 0;
}

process.exit(await main());
