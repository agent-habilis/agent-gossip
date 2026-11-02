#!/usr/bin/env bun
// `macro_rules!` and proc macros are refused, except the ones that `macro-rules.allow` names. Each row
// gives the file and the exact count of macros in it, with a reason. A new macro fails the check, and
// so does a count that goes up or down, so a removed macro must also leave the allow list.
// `macro-rules.test.ts` plants one file of each kind.

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const allowFile = process.env.MACRO_RULES_ALLOW || "scripts/macro-rules/macro-rules.allow";

const MACRO = /\bmacro_rules!|#\[proc_macro(_attribute|_derive)?\]/g;
const SCOPE = /^crates\/.*\.rs$/;

// The files git knows that sit under crates/: tracked, or new and not ignored.
function listFiles(): string[] {
  const proc = spawnSync("git", ["ls-files", "-z", "-co", "--exclude-standard"], {
    cwd: ROOT,
    encoding: "utf8",
  });
  return (proc.stdout ?? "").split("\0").filter((file) => SCOPE.test(file));
}

function countIn(file: string): number {
  const text = readFileSync(path.join(ROOT, file), "utf8");
  return text.match(MACRO)?.length ?? 0;
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
      problems.push(`macro-rules: ${allowFile} has a row for ${target} with no reason. Say why the macro stays.`);
    }
    if (allowed.has(target)) {
      problems.push(`macro-rules: ${allowFile} has two rows for ${target}. Merge them.`);
    }
    allowed.set(target, Number(count));
  }
  return { allowed, problems };
}

function main(): number {
  const { allowed, problems } = readAllow();
  const found = new Map<string, number>();
  for (const file of listFiles()) {
    const count = countIn(file);
    if (count > 0) found.set(file, count);
  }
  for (const [file, count] of [...found].sort()) {
    const expected = allowed.get(file);
    if (expected === undefined) {
      problems.push(
        `macro-rules: ${file}: ${count} macro_rules! not allowed. Write a fn, a generic or a trait, or add a row to ${allowFile} with a reason.`,
      );
    } else if (expected !== count) {
      problems.push(
        `macro-rules: ${file}: ${count} macro_rules! found, ${expected} allowed. Set the count in ${allowFile}.`,
      );
    }
  }
  for (const file of [...allowed.keys()].sort()) {
    if (!found.has(file)) {
      problems.push(`macro-rules: ${file}: stale row. No macro_rules! is left in it. Remove it from ${allowFile}.`);
    }
  }
  for (const problem of problems) console.log(problem);
  return problems.length > 0 ? 1 : 0;
}

process.exit(main());
