#!/usr/bin/env bun
// Scripts are written in TypeScript and run with Bun (AGENTS.md, "Scripts"). This check fails on a
// script in another language that the repository holds: a file with a shell, Python or JavaScript
// extension, or a file of any name whose first line is a shebang for one of those interpreters.
// `script-language.allow` names a file that may stay, with a reason. A row that matches nothing is
// stale and fails too. `script-language.test.ts` plants one script of each kind.

import { spawnSync } from "node:child_process";
import { closeSync, existsSync, openSync, readFileSync, readSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const allowFile = process.env.SCRIPT_LANGUAGE_ALLOW || "scripts/script-language/script-language.allow";

const EXTENSION = /\.(sh|bash|zsh|ksh|dash|fish|py|js|mjs|cjs)$/;
const INTERPRETER = /^(sh|bash|zsh|ksh|dash|fish|python[0-9.]*|node|nodejs|deno)$/;

// The interpreter a shebang line names: `#!/usr/bin/env -S bash -e` and `#!/usr/local/bin/bash`
// both give `bash`. Words after `env` that are flags or NAME=value belong to `env`, not to it.
function interpreterOf(firstLine: string): string | undefined {
  if (!firstLine.startsWith("#!")) return undefined;
  const words = firstLine.slice(2).trim().split(/\s+/);
  let command = path.basename(words[0] ?? "");
  let rest = words.slice(1);
  if (command === "env") {
    rest = rest.filter((word) => !word.startsWith("-") && !word.includes("="));
    command = path.basename(rest[0] ?? "");
  }
  return command;
}

// The files git knows: tracked, or new and not ignored. -z keeps a file name with a space whole.
function listFiles(): string[] {
  const proc = spawnSync("git", ["ls-files", "-z", "-co", "--exclude-standard"], {
    cwd: ROOT,
    encoding: "utf8",
  });
  return (proc.stdout ?? "").split("\0").filter((file) => file.length > 0);
}

// The first bytes of a file are enough for a shebang line, and a binary file is never read whole.
function readHead(file: string): string {
  const fd = openSync(file, "r");
  try {
    const buffer = Buffer.alloc(256);
    const length = readSync(fd, buffer, 0, buffer.length, 0);
    return buffer.toString("utf8", 0, length);
  } finally {
    closeSync(fd);
  }
}

// The rule each file breaks, if any. Any file is read for its shebang, whatever its extension.
function violationOf(file: string): string | undefined {
  if (EXTENSION.test(file)) return "extension";
  if (!existsSync(path.join(ROOT, file))) return undefined;
  const firstLine = readHead(path.join(ROOT, file)).split("\n")[0] ?? "";
  return INTERPRETER.test(interpreterOf(firstLine) ?? "") ? "shebang" : undefined;
}

function readAllow(): { allowed: Map<string, string>; problems: string[] } {
  const allowed = new Map<string, string>();
  const problems: string[] = [];
  const file = path.join(ROOT, allowFile);
  if (!existsSync(file)) return { allowed, problems };
  for (const line of readFileSync(file, "utf8").split("\n")) {
    if (/^\s*(#|$)/.test(line)) continue;
    const [rule, target] = line.split("|");
    const key = `${rule}|${target}`;
    if (allowed.has(key)) {
      problems.push(`script-language: ${allowFile} has two rows for [${rule}] ${target}. Merge them.`);
    }
    allowed.set(key, line);
  }
  return { allowed, problems };
}

function main(): number {
  const { allowed, problems } = readAllow();
  const found = new Set<string>();
  for (const file of listFiles()) {
    const rule = violationOf(file);
    if (!rule) continue;
    const key = `${rule}|${file}`;
    found.add(key);
    if (!allowed.has(key)) {
      problems.push(
        `script-language: [${rule}] ${file}: a shell, Python or JavaScript script. Write it in TypeScript and run it with Bun, or add a row to ${allowFile} with a reason.`,
      );
    }
  }
  for (const key of [...allowed.keys()].sort()) {
    if (!found.has(key)) {
      const [rule, file] = key.split("|");
      problems.push(`script-language: [${rule}] ${file}: stale row. Nothing matches it. Remove it from ${allowFile}.`);
    }
  }
  for (const problem of problems) console.log(problem);
  return problems.length > 0 ? 1 : 0;
}

process.exit(main());
