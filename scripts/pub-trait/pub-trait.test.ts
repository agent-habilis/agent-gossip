// Tests of scripts/pub-trait/pub-trait.ts. Each case plants files in a scratch git repository, with
// its own allow list, and runs the check there. A trait the check misses, or a clean file it
// refuses, fails here.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, expect, test } from "bun:test";

const here = path.dirname(fileURLToPath(import.meta.url));
const work = mkdtempSync(path.join(tmpdir(), "pub-trait-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));

// Plant each file in a fresh repository with `allow` as its allow list, then run the check.
function check(files: Record<string, string>, allow = ""): { status: number; out: string } {
  const repo = mkdtempSync(path.join(work, "repo-"));
  mkdirSync(path.join(repo, "scripts", "pub-trait"), { recursive: true });
  copyFileSync(path.join(here, "pub-trait.ts"), path.join(repo, "scripts", "pub-trait", "pub-trait.ts"));
  writeFileSync(path.join(repo, "scripts", "pub-trait", "pub-trait.allow"), `${allow}\n`);
  for (const [file, content] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(repo, file)), { recursive: true });
    writeFileSync(path.join(repo, file), `${content}\n`);
  }
  spawnSync("git", ["init", "-q"], { cwd: repo });
  const proc = spawnSync(process.execPath, [path.join(repo, "scripts", "pub-trait", "pub-trait.ts")], {
    cwd: repo,
    encoding: "utf8",
  });
  return { status: proc.status ?? -1, out: `${proc.stdout ?? ""}${proc.stderr ?? ""}` };
}

const expectExit = (files: Record<string, string>, expected: number, allow = "") => {
  const run = check(files, allow);
  expect(run.status, run.out).toBe(expected);
};

const trait = "pub trait Sink {}";

test("a new pub trait is refused", () => {
  expectExit({ "crates/x/src/a.rs": trait }, 1);
});

test("a pub trait that the allow list names with its exact count passes", () => {
  expectExit({ "crates/x/src/a.rs": trait }, 0, "crates/x/src/a.rs|1|a generic bound");
});

test("two pub traits in one file need a count of two", () => {
  expectExit({ "crates/x/src/a.rs": `${trait}\n${trait}` }, 1, "crates/x/src/a.rs|1|a generic bound");
  expectExit({ "crates/x/src/a.rs": `${trait}\n${trait}` }, 0, "crates/x/src/a.rs|2|a generic bound");
});

test("a count too high is refused", () => {
  expectExit({ "crates/x/src/a.rs": trait }, 1, "crates/x/src/a.rs|2|a generic bound");
});

test("a row with no pub trait left is stale", () => {
  expectExit({ "crates/x/src/a.rs": "fn f() {}" }, 1, "crates/x/src/a.rs|1|a generic bound");
});

test("a crate-private trait is clean", () => {
  expectExit({ "crates/x/src/a.rs": "pub(crate) trait Sink {}" }, 0);
});

test("a pub trait outside crates is not counted", () => {
  expectExit({ "scripts/a.rs": trait }, 0);
});

test("two rows for one file are refused", () => {
  expectExit({ "crates/x/src/a.rs": trait }, 1, "crates/x/src/a.rs|1|a\ncrates/x/src/a.rs|1|b");
});
