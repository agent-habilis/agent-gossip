// Tests of scripts/naming/naming.ts. Each case plants files in a scratch git repository, with its
// own allow list, and runs the check there. A name the check misses, or a clean file it refuses,
// fails here.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, expect, test } from "bun:test";

const here = path.dirname(fileURLToPath(import.meta.url));
const work = mkdtempSync(path.join(tmpdir(), "naming-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));

// Plant each file in a fresh repository with `allow` as its allow list, then run the check.
function check(files: Record<string, string>, allow = ""): { status: number; out: string } {
  const repo = mkdtempSync(path.join(work, "repo-"));
  mkdirSync(path.join(repo, "scripts", "naming"), { recursive: true });
  copyFileSync(path.join(here, "naming.ts"), path.join(repo, "scripts", "naming", "naming.ts"));
  writeFileSync(path.join(repo, "scripts", "naming", "naming.allow"), `${allow}\n`);
  for (const [file, content] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(repo, file)), { recursive: true });
    writeFileSync(path.join(repo, file), `${content}\n`);
  }
  spawnSync("git", ["init", "-q"], { cwd: repo });
  const proc = spawnSync(process.execPath, [path.join(repo, "scripts", "naming", "naming.ts")], {
    cwd: repo,
    encoding: "utf8",
  });
  return { status: proc.status ?? -1, out: `${proc.stdout ?? ""}${proc.stderr ?? ""}` };
}

const expectExit = (files: Record<string, string>, expected: number, allow = "") => {
  const run = check(files, allow);
  expect(run.status, run.out).toBe(expected);
};

const file = "crates/x/src/a.rs";

test("a new_<x> constructor with no plain new is refused", () => {
  expectExit({ [file]: "impl A {\n    pub fn new_with_id(id: u8) -> Self { A(id) }\n}" }, 1);
});

test("a plain new beside new_<x> is clean", () => {
  expectExit({ [file]: "impl A {\n    pub fn new() -> Self { A(0) }\n    pub fn new_with_id(id: u8) -> Self { A(id) }\n}" }, 0);
});

test("a counted new_<x> passes", () => {
  expectExit({ [file]: "impl A {\n    pub fn new_with_id(id: u8) -> Self { A(id) }\n}" }, 0, `ctor_name|${file}|1|a fixture`);
});

test("a row with no reason is refused", () => {
  expectExit({ [file]: "impl A {\n    pub fn new_with_id(id: u8) -> Self { A(id) }\n}" }, 1, `ctor_name|${file}|1|`);
});

test("a row for a file that is clean now is stale", () => {
  expectExit({ [file]: "impl A {\n    pub fn new() -> Self { A(0) }\n}" }, 1, `ctor_name|${file}|1|a fixture`);
});

test("a row for an unknown rule is refused", () => {
  expectExit({ [file]: "fn f() {}" }, 1, `no_such_rule|${file}|1|a fixture`);
});

test("a from_<x> fn with no impl From is refused", () => {
  expectExit({ [file]: "impl A {\n    pub fn from_bytes(b: u8) -> Self { A(b) }\n}" }, 1);
});

test("a from_<x> fn beside an impl From is clean", () => {
  expectExit({ [file]: "impl From<u8> for A {\n    fn from(b: u8) -> Self { A(b) }\n}\nimpl A {\n    pub fn from_bytes(b: u8) -> Self { A(b) }\n}" }, 0);
});

test("a counted from_<x> fn passes", () => {
  expectExit({ [file]: "impl A {\n    pub fn from_bytes(b: u8) -> Self { A(b) }\n}" }, 0, `from_name|${file}|1|a fixture`);
});
