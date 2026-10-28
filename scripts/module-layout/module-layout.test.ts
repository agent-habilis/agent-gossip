// Tests of scripts/module-layout/module-layout.ts. Each case plants files in a scratch git
// repository, with its own allow list, and runs the check there. A layout the check misses, or a
// clean file it refuses, fails here.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, expect, test } from "bun:test";

const here = path.dirname(fileURLToPath(import.meta.url));
const work = mkdtempSync(path.join(tmpdir(), "module-layout-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));

// Plant each file in a fresh repository with `allow` as its allow list, then run the check.
function check(files: Record<string, string>, allow = ""): { status: number; out: string } {
  const repo = mkdtempSync(path.join(work, "repo-"));
  mkdirSync(path.join(repo, "scripts", "module-layout"), { recursive: true });
  copyFileSync(
    path.join(here, "module-layout.ts"),
    path.join(repo, "scripts", "module-layout", "module-layout.ts"),
  );
  writeFileSync(path.join(repo, "scripts", "module-layout", "module-layout.allow"), `${allow}\n`);
  for (const [file, content] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(repo, file)), { recursive: true });
    writeFileSync(path.join(repo, file), `${content}\n`);
  }
  spawnSync("git", ["init", "-q"], { cwd: repo });
  const proc = spawnSync(
    process.execPath,
    [path.join(repo, "scripts", "module-layout", "module-layout.ts")],
    { cwd: repo, encoding: "utf8" },
  );
  return { status: proc.status ?? -1, out: `${proc.stdout ?? ""}${proc.stderr ?? ""}` };
}

const expectExit = (files: Record<string, string>, expected: number, allow = "") => {
  const run = check(files, allow);
  expect(run.status, run.out).toBe(expected);
};

const leaf = "crates/x/src/frame_cache.rs";

test("a leaf named for none of its public types is refused", () => {
  expectExit({ [leaf]: "pub struct Other;" }, 1);
});

test("a leaf named for its public type passes", () => {
  expectExit({ [leaf]: "pub struct FrameCache;" }, 0);
});

test("a leaf with no public type is fine", () => {
  expectExit({ [leaf]: "fn helper() {}" }, 0);
});

test("a mod.rs that only declares modules is not a leaf", () => {
  expectExit({ "crates/x/src/mod.rs": "pub mod inner;\npub use inner::Other;" }, 0);
});

const constLines = (count: number) =>
  Array.from({ length: count }, (_, i) => `const C${i}: u8 = ${i};`).join("\n");

test("a mod.rs that defines more than 20 lines of items is refused", () => {
  expectExit({ "crates/x/src/mod.rs": `pub mod inner;\n${constLines(21)}` }, 1);
});

test("a lib.rs that defines more than 20 lines of items is refused", () => {
  expectExit({ "crates/x/src/lib.rs": constLines(21) }, 1);
});

test("a mod.rs with items only under cfg(test) is clean", () => {
  expectExit({ "crates/x/src/mod.rs": "pub mod inner;\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}" }, 0);
});

test("a mod.rs with 20 lines of items is clean (O13)", () => {
  expectExit({ "crates/x/src/mod.rs": `pub mod inner;\n${constLines(20)}` }, 0);
});

test("a mod.rs with 21 lines of items is refused (O13)", () => {
  expectExit({ "crates/x/src/mod.rs": `pub mod inner;\n${constLines(21)}` }, 1);
});

test("a counted mod.rs passes", () => {
  expectExit({ "crates/x/src/mod.rs": constLines(21) }, 0, "crates/x/src/mod.rs|1|items to move");
});

test("a file with a child module is not a leaf", () => {
  expectExit({ [leaf]: "pub mod inner;\npub struct Other;" }, 0);
});

test("a counted mismatch passes", () => {
  expectExit({ [leaf]: "pub struct Other;" }, 0, `${leaf}|1|renaming pending`);
});

test("a row with no reason is refused", () => {
  expectExit({ [leaf]: "pub struct Other;" }, 1, `${leaf}|1|`);
});

test("a row for a file that is fine now is stale", () => {
  expectExit({ [leaf]: "pub struct FrameCache;" }, 1, `${leaf}|1|renaming pending`);
});

test("tests are not scanned", () => {
  expectExit({ "crates/x/tests/frame_cache.rs": "pub struct Other;" }, 0);
});
