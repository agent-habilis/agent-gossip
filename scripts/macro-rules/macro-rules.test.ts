// Tests of scripts/macro-rules/macro-rules.ts. Each case plants files in a scratch git repository,
// with its own allow list, and runs the check there. A macro the check misses, or a clean file it
// refuses, fails here.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, expect, test } from "bun:test";

const here = path.dirname(fileURLToPath(import.meta.url));
const work = mkdtempSync(path.join(tmpdir(), "macro-rules-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));

// Plant each file in a fresh repository with `allow` as its allow list, then run the check.
function check(files: Record<string, string>, allow = ""): { status: number; out: string } {
  const repo = mkdtempSync(path.join(work, "repo-"));
  mkdirSync(path.join(repo, "scripts", "macro-rules"), { recursive: true });
  copyFileSync(path.join(here, "macro-rules.ts"), path.join(repo, "scripts", "macro-rules", "macro-rules.ts"));
  writeFileSync(path.join(repo, "scripts", "macro-rules", "macro-rules.allow"), `${allow}\n`);
  for (const [file, content] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(repo, file)), { recursive: true });
    writeFileSync(path.join(repo, file), `${content}\n`);
  }
  spawnSync("git", ["init", "-q"], { cwd: repo });
  const proc = spawnSync(process.execPath, [path.join(repo, "scripts", "macro-rules", "macro-rules.ts")], {
    cwd: repo,
    encoding: "utf8",
  });
  return { status: proc.status ?? -1, out: `${proc.stdout ?? ""}${proc.stderr ?? ""}` };
}

const expectExit = (files: Record<string, string>, expected: number, allow = "") => {
  const run = check(files, allow);
  expect(run.status, run.out).toBe(expected);
};

const macro = "macro_rules! twice { ($e:expr) => { $e * 2 }; }";

test("a new macro_rules! in crates is refused", () => {
  expectExit({ "crates/x/src/a.rs": macro }, 1);
});

test("a macro that the allow list names with its exact count passes", () => {
  expectExit({ "crates/x/src/a.rs": macro }, 0, "crates/x/src/a.rs|1|a fixture");
});

test("two macros in one file need a count of two", () => {
  expectExit({ "crates/x/src/a.rs": `${macro}\n${macro}` }, 1, "crates/x/src/a.rs|1|a fixture");
  expectExit({ "crates/x/src/a.rs": `${macro}\n${macro}` }, 0, "crates/x/src/a.rs|2|a fixture");
});

test("a count too high is refused", () => {
  expectExit({ "crates/x/src/a.rs": macro }, 1, "crates/x/src/a.rs|2|a fixture");
});

test("a row with no macro left is stale", () => {
  expectExit({ "crates/x/src/a.rs": "fn f() {}" }, 1, "crates/x/src/a.rs|1|a fixture");
});

test("a macro outside crates is not counted", () => {
  expectExit({ "scripts/a.rs": macro }, 0);
});

test("a proc macro in crates is refused", () => {
  expectExit({ "crates/x/src/a.rs": "#[proc_macro_attribute]\npub fn test(a: TokenStream, b: TokenStream) -> TokenStream { b }" }, 1);
});

test("a row with no reason is refused", () => {
  expectExit({ "crates/x/src/a.rs": macro }, 1, "crates/x/src/a.rs|1|");
});

test("two rows for one file are refused", () => {
  expectExit({ "crates/x/src/a.rs": macro }, 1, "crates/x/src/a.rs|1|a\ncrates/x/src/a.rs|1|b");
});
