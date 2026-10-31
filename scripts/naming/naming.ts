#!/usr/bin/env bun
// Naming rules of the Rust style guide that no lint checks (issue #29). Each rule is a per-file
// proxy. A file that breaks a rule needs a counted row in `naming.allow` with its reason, and the
// row is exact: the check fails on a new hit, on a count that differs, and on a stale row.
//   ctor_name: a file with `fn new_<x>` and no plain `fn new` (N10: `new` or `new_with_foo`).
//   from_name: a file with `fn from_<x>` and no `impl From<` (N12: `from_foo` pairs with a From).
// `naming.test.ts` plants one file of each kind.

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const allowFile = process.env.NAMING_ALLOW || "scripts/naming/naming.allow";

const rules: Record<string, (src: string) => boolean> = {
  ctor_name: (src) => /\bfn new_\w+\s*[(<]/.test(src) && !/\bfn new\s*[(<]/.test(src),
  from_name: (src) => /\bfn from_\w+\s*[(<]/.test(src) && !/impl(<[^>]*>)? From</.test(src),
};

const HINT: Record<string, string> = {
  ctor_name: "a constructor named new_<x> needs a plain new in the same file",
  from_name: "a fn named from_<x> needs an impl From in the same file",
};

// The Rust files git knows, tracked or not yet, minus what .gitignore hides.
function listFiles(): string[] {
  const proc = spawnSync("git", ["ls-files", "-z", "-co", "--exclude-standard"], {
    cwd: ROOT,
    encoding: "utf8",
  });
  return (proc.stdout ?? "").split("\0").filter((file) => file.endsWith(".rs"));
}

// rule|file|count|reason. Blank lines and `#` comments are skipped.
function readAllow(): { allowed: Map<string, number>; problems: string[] } {
  const allowed = new Map<string, number>();
  const problems: string[] = [];
  const file = path.join(ROOT, allowFile);
  if (!existsSync(file)) return { allowed, problems };
  for (const line of readFileSync(file, "utf8").split("\n")) {
    if (/^\s*(#|$)/.test(line)) continue;
    const [rule = "", target = "", count = "", reason = ""] = line.split("|");
    const key = `${rule}|${target}`;
    if (!(rule in rules)) problems.push(`naming: ${allowFile} has a row for an unknown rule ${rule}.`);
    if (reason.trim() === "") problems.push(`naming: ${allowFile} has a row for ${key} with no reason.`);
    if (allowed.has(key)) problems.push(`naming: ${allowFile} has two rows for ${key}. Merge them.`);
    allowed.set(key, Number(count));
  }
  return { allowed, problems };
}

function main(): number {
  const { allowed, problems } = readAllow();
  const found = new Map<string, number>();
  for (const file of listFiles()) {
    const src = readFileSync(path.join(ROOT, file), "utf8");
    for (const [rule, hits] of Object.entries(rules)) {
      if (hits(src)) found.set(`${rule}|${file}`, 1);
    }
  }
  for (const [key, count] of [...found].sort()) {
    const expected = allowed.get(key);
    const [rule, file] = key.split("|");
    if (expected === undefined) {
      problems.push(`naming: [${rule}] ${file}: ${HINT[rule ?? ""] ?? ""}. Fix it, or add a row to ${allowFile} with the reason.`);
    } else if (expected !== count) {
      problems.push(`naming: [${rule}] ${file}: ${count} hit found, ${expected} allowed. Set the count in ${allowFile}.`);
    }
  }
  for (const key of [...allowed.keys()].sort()) {
    if (!found.has(key)) {
      const [rule, file] = key.split("|");
      problems.push(`naming: [${rule}] ${file}: stale row. The file no longer breaks the rule. Remove it from ${allowFile}.`);
    }
  }
  for (const problem of problems) console.log(problem);
  return problems.length > 0 ? 1 : 0;
}

process.exit(main());
