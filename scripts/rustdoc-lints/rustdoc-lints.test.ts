// Tests of the doc lints in the root Cargo.toml (D10, D11). Each case plants one doc comment in a
// scratch crate that takes the `[workspace.lints.rustdoc]` table and the doc lints of
// `[workspace.lints.clippy]` from the root Cargo.toml, and runs rustdoc or clippy on it. A lint that
// the root Cargo.toml drops, or that stops firing, fails here.

import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, expect, setDefaultTimeout, test } from "bun:test";

// The first cargo call on a fresh CI runner resolves the toolchain and the registry index, and it took more
// than the 5 s default of bun test.
setDefaultTimeout(120_000);

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const work = mkdtempSync(path.join(tmpdir(), "rustdoc-lints-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));

const rootManifest = readFileSync(path.join(root, "Cargo.toml"), "utf8");
const rustdocTable = /^\[workspace\.lints\.rustdoc\]\n((?:[^\[\n].*\n|\n(?=[^\[]))*)/m.exec(rootManifest)?.[1] ?? "";
const clippyDocLints = ["too_long_first_doc_paragraph", "doc_include_without_cfg"]
  .map((lint) => new RegExp(`^${lint} = "warn"$`, "m").exec(rootManifest)?.[0] ?? "")
  .join("\n");

// A scratch crate with `lib` as src/lib.rs, then the command. The shared target dir keeps a case fast.
function run(tool: "doc" | "clippy", lib: string): { status: number; out: string } {
  const crate = mkdtempSync(path.join(work, "crate-"));
  mkdirSync(path.join(crate, "src"));
  const manifest = `[package]\nname = "probe"\nversion = "0.0.0"\nedition = "2021"\n\n[lints.rustdoc]\n${rustdocTable}\n[lints.clippy]\n${clippyDocLints}\n`;
  writeFileSync(path.join(crate, "Cargo.toml"), manifest);
  writeFileSync(path.join(crate, "src", "lib.rs"), lib);
  const args = tool === "doc" ? ["doc", "--no-deps", "--document-private-items"] : ["clippy", "--", "-D", "warnings"];
  const proc = spawnSync("cargo", args, {
    cwd: crate,
    encoding: "utf8",
    env: { ...process.env, CARGO_TARGET_DIR: path.join(work, "target"), RUSTDOCFLAGS: "-D warnings" },
  });
  return { status: proc.status ?? -1, out: `${proc.stdout ?? ""}${proc.stderr ?? ""}` };
}

const docFails = (lib: string, lint: string) => {
  const result = run("doc", lib);
  expect(result.status, result.out).not.toBe(0);
  expect(result.out).toContain(lint);
};

test("the root Cargo.toml holds the rustdoc table and the clippy doc lints", () => {
  expect(rustdocTable).toContain("broken_intra_doc_links");
  expect(clippyDocLints).toContain("too_long_first_doc_paragraph");
});
test("rustdoc: a clean doc comment passes", () => {
  const result = run("doc", "//! Crate.\n\n/// Does a thing.\npub fn f() {}\n");
  expect(result.status, result.out).toBe(0);
});
test("rustdoc: a broken link fails", () => {
  docFails("//! Crate.\n\n/// See [`nope`].\npub fn f() {}\n", "broken-intra-doc-links");
});
test("rustdoc: a link to a private item from a public doc fails", () => {
  docFails("//! Crate.\n\nfn hidden() {}\n\n/// See [`hidden`].\npub fn f() {}\n", "private-intra-doc-links");
});
test("rustdoc: a bare URL fails", () => {
  docFails("//! Crate.\n\n/// See https://example.com now.\npub fn f() {}\n", "bare-urls");
});
test("rustdoc: an unclosed HTML tag fails", () => {
  docFails("//! Crate.\n\n/// A <div> that never closes.\npub fn f() {}\n", "invalid-html-tags");
});
test("rustdoc: an unescaped backtick fails", () => {
  docFails("//! Crate.\n\n/// Use `a`b` here.\npub fn f() {}\n", "unescaped-backticks");
});
test("rustdoc: a redundant explicit link fails", () => {
  docFails("//! Crate.\n\n/// See [`g`](g).\npub fn f() {}\n\n/// Other.\npub fn g() {}\n", "redundant-explicit-links");
});
test("rustdoc: an invalid code block attribute fails", () => {
  docFails("//! Crate.\n\n/// ```should-panic\n/// let _ = 1;\n/// ```\npub fn f() {}\n", "invalid-codeblock-attributes");
});
test("clippy: a first paragraph of a doc comment that runs long fails", () => {
  const long = Array.from({ length: 12 }, () => "/// A sentence that goes on and on without a break.").join("\n");
  const result = run("clippy", `//! Crate.\n\n${long}\npub fn f() {}\n`);
  expect(result.status, result.out).not.toBe(0);
  expect(result.out).toContain("too_long_first_doc_paragraph");
});
test("clippy: a short summary line then a long body passes", () => {
  const long = Array.from({ length: 12 }, () => "/// A sentence that goes on and on without a break.").join("\n");
  const result = run("clippy", `//! Crate.\n\n/// Does a thing.\n///\n${long}\npub fn f() {}\n`);
  expect(result.status, result.out).toBe(0);
});
