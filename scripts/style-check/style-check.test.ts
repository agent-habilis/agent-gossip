// Tests of scripts/style-check/style-check.ts. Each case plants a file in a scratch git repository
// and runs the check there. A rule that the check does not catch, or a clean file that it refuses,
// fails here.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, expect, test } from "bun:test";

const here = path.dirname(fileURLToPath(import.meta.url));
const work = mkdtempSync(path.join(tmpdir(), "style-check-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));

// Plant `content` at `file` in a fresh repository with `allow` as its allow list, then run the check.
function check(file: string, content: string, allow = ""): { status: number; out: string } {
  const repo = mkdtempSync(path.join(work, "repo-"));
  mkdirSync(path.join(repo, "scripts", "style-check"), { recursive: true });
  copyFileSync(path.join(here, "style-check.ts"), path.join(repo, "scripts", "style-check", "style-check.ts"));
  writeFileSync(path.join(repo, "scripts", "style-check", "style-check.allow"), `${allow}\n`);
  mkdirSync(path.dirname(path.join(repo, file)), { recursive: true });
  // A doc example in the demo crate, so the crate_example rule does not fire on every case.
  mkdirSync(path.join(repo, "crates", "demo", "src"), { recursive: true });
  writeFileSync(path.join(repo, "crates", "demo", "src", "example.rs"), "//! ```\n//! ```\n");
  writeFileSync(path.join(repo, file), `${content}\n`);
  spawnSync("git", ["init", "-q"], { cwd: repo });
  const proc = spawnSync(process.execPath, [path.join(repo, "scripts", "style-check", "style-check.ts")], {
    cwd: repo,
    encoding: "utf8",
  });
  return { status: proc.status ?? -1, out: `${proc.stdout ?? ""}${proc.stderr ?? ""}` };
}

const expectExit = (file: string, content: string, expected: number, allow = "") => {
  const run = check(file, content, allow);
  expect(run.status, run.out).toBe(expected);
};

const violation = "fn f() { let _ = std::panic::catch_unwind(|| 1); }";

// Each rule catches its plain form.
test("poison: path form", () => {
  expectExit("src/a.rs", "fn f(m: &M) { m.lock().unwrap_or_else(std::sync::PoisonError::into_inner); }", 1);
});
test("poison: closure form", () => {
  expectExit("src/a.rs", "fn f(m: &M) { m.lock().unwrap_or_else(|e| e.into_inner()); }", 1);
});
test("poison: renamed import", () => {
  expectExit("src/a.rs", "use std::sync::PoisonError as Pe;", 1);
});
test("poison: turbofish", () => {
  expectExit("src/a.rs", "fn f() { let _ = PoisonError::<u8>::into_inner; }", 1);
});
test("feature: plain", () => {
  expectExit("src/a.rs", "#![feature(never_type)]", 1);
});
test("feature: behind cfg_attr", () => {
  expectExit("src/a.rs", "#![cfg_attr(nightly, feature(coroutines))]", 1);
});
test("catch_unwind", () => {
  expectExit("src/a.rs", "fn f() { let _ = std::panic::catch_unwind(|| 1); }", 1);
});
test("ctor: attribute", () => {
  expectExit("src/a.rs", "#[ctor]\nfn init() {}", 1);
});
test("ctor: path attribute", () => {
  expectExit("src/a.rs", "#[ctor::ctor]\nfn init() {}", 1);
});
test("block doc: outer", () => {
  expectExit("src/a.rs", "/** old style */\nfn f() {}", 1);
});
test("block doc: inner", () => {
  expectExit("src/a.rs", "/*! crate doc */", 1);
});

// A type lifetime is named for what it borrows. A single letter is allowed on a fn.
test("lifetime: single letter on a struct", () => {
  expectExit("src/a.rs", "pub struct Frame<'a>(&'a [u8]);", 1);
});
test("lifetime: single letter on an enum", () => {
  expectExit("src/a.rs", "enum Keying<'a> { Shared(&'a u8) }", 1);
});
test("lifetime: descriptive name on a struct", () => {
  expectExit("src/a.rs", "pub struct Frame<'frame>(&'frame [u8]);", 0);
});
test("lifetime: serde de on a struct", () => {
  expectExit("src/a.rs", "struct Visitor<'de>(PhantomData<&'de ()>);", 0);
});
test("lifetime: single letter on a fn", () => {
  expectExit("src/a.rs", "fn first<'a>(x: &'a [u8]) -> &'a u8 { &x[0] }", 0);
});

// Glob imports: no form is exempt, not `use super::*` in a test module and not a `pub use` facade.
test("glob: use super", () => {
  expectExit("src/a.rs", "#[cfg(test)]\nmod tests {\n    use super::*;\n}", 1);
});
test("glob: pub use facade", () => {
  expectExit("src/a.rs", "pub use inner::*;", 1);
});
test("glob: third-party prelude", () => {
  expectExit("src/a.rs", "use wasm_bindgen::prelude::*;", 1);
});
test("glob: inside braces", () => {
  expectExit("src/a.rs", "use inner::{*, Thing};", 1);
});
test("glob: explicit import", () => {
  expectExit("src/a.rs", "use inner::{Thing, other::Other};", 0);
});
test("glob: a comment that holds a glob", () => {
  expectExit("src/a.rs", "// use super::*;", 0);
});
test("glob: counted exception", () => {
  expectExit("src/a.rs", "use super::*;", 0, "glob|src/a.rs|1|test exception");
});

// A clean file passes, and so does a string that only looks like a block doc comment.
test("clean file", () => {
  expectExit("src/a.rs", "fn f() {}", 0);
});
test("string with /**", () => {
  expectExit("src/a.rs", 'const G: &str = "src/**";', 0);
});
test("doc comment with ///", () => {
  expectExit("src/a.rs", "/// A doc comment.\nfn f() {}", 0);
});

// The allow list.
test("file name with a space", () => {
  expectExit("c/src/a b.rs", violation, 1);
});
test("allowed, exact count", () => {
  expectExit("src/a.rs", violation, 0, "catch_unwind|src/a.rs|1|a boundary");
});
test("allowed, count exceeded", () => {
  expectExit("src/a.rs", `${violation}\n${violation}`, 1, "catch_unwind|src/a.rs|1|a boundary");
});
test("allowed, count too high (stale)", () => {
  expectExit("src/a.rs", violation, 1, "catch_unwind|src/a.rs|2|a boundary");
});
test("allowed, file has no match (stale)", () => {
  expectExit("src/a.rs", "fn f() {}", 1, "catch_unwind|src/a.rs|1|a boundary");
});
test("two rows for one file", () => {
  expectExit("src/a.rs", violation, 1, "catch_unwind|src/a.rs|1|one\ncatch_unwind|src/a.rs|1|two");
});
test("comment and blank lines in the list", () => {
  expectExit("src/a.rs", violation, 0, "# a comment\n\ncatch_unwind|src/a.rs|1|a boundary");
});

// The anyhow_pub rule: a public signature that returns anyhow, in a library file.
const anyhowFixture = [
  "pub fn a() -> anyhow::Result<()> { Ok(()) }",
  "pub fn b(",
  "    x: u8,",
  ") -> anyhow::Result<",
  "    u8,",
  "> {",
  "    Ok(x)",
  "}",
  "pub(crate) fn c() -> anyhow::Result<()> { Ok(()) }",
  "pub trait T {",
  "    fn d(&self) -> anyhow::Result<()>;",
  "}",
  "impl std::str::FromStr for X {",
  "    type Err = anyhow::Error;",
  "    fn from_str(_: &str) -> Result<Self, Self::Err> { unimplemented!() }",
  "}",
  "use anyhow::Result;",
  "pub fn g() -> Result<u8> { Ok(1) }",
  "#[cfg(test)]",
  "mod tests {",
  "    pub fn e() -> anyhow::Result<()> { Ok(()) }",
  "}",
].join("\n");
const anyhowMain = "pub fn m() -> anyhow::Result<()> { Ok(()) }";
test("anyhow_pub: the Result import counts when a public fn returns the bare alias", () => {
  expectExit("crates/demo/src/lib.rs", "use anyhow::Result;\npub fn e() -> Result<u8> { Ok(1) }", 1);
});
test("anyhow_pub: the Result import does not count without a public bare-alias fn", () => {
  expectExit("crates/demo/src/lib.rs", "use anyhow::Result;\npub(crate) fn f() -> Result<u8> { Ok(1) }\nfn g() -> Result<u8> { Ok(1) }", 0);
});
test("anyhow_pub: six public forms (the test module included), not the pub(crate) or the binary", () => {
  expectExit("crates/demo/src/lib.rs", anyhowFixture, 1);
  expectExit("crates/demo/src/main.rs", anyhowMain, 0);
});
test("anyhow_pub: the exact count is allowed", () => {
  expectExit("crates/demo/src/lib.rs", anyhowFixture, 0, "anyhow_pub|crates/demo/src/lib.rs|6|fixture");
});
test("anyhow_pub: one more than allowed fails", () => {
  expectExit("crates/demo/src/lib.rs", anyhowFixture.replace("#[cfg(test)]", "pub fn z() -> anyhow::Result<()> { Ok(()) }\n#[cfg(test)]"), 1, "anyhow_pub|crates/demo/src/lib.rs|6|fixture");
});
test("anyhow_pub: a stale count fails", () => {
  expectExit("crates/demo/src/lib.rs", anyhowFixture, 1, "anyhow_pub|crates/demo/src/lib.rs|7|fixture");
});

// The advisor's review of the anyhow_pub rule: one case per hole, each failing on the old rule.
test("anyhow_pub: a bare Result fn counts beside the import, once per fn", () => {
  expectExit("crates/demo/src/lib.rs", "use anyhow::Result;\npub fn f() -> Result<u8> { Ok(1) }\npub fn g() -> Result<u8> { Ok(2) }", 0, "anyhow_pub|crates/demo/src/lib.rs|2|fixture");
});
test("anyhow_pub: a test module strips only its own body", () => {
  expectExit("crates/demo/src/lib.rs", "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\npub fn late() -> anyhow::Result<()> { Ok(()) }", 0, "anyhow_pub|crates/demo/src/lib.rs|1|fixture");
});
test("anyhow_pub: a commented-out signature does not count", () => {
  expectExit("crates/demo/src/lib.rs", "// pub fn f() -> anyhow::Result<()> { Ok(()) }", 0);
});
test("anyhow_pub: the error may be second, and the alias may sit in a Future", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> Result<(), anyhow::Error> { Ok(()) }\npub trait T {\n    fn g(&self) -> impl Future<Output = anyhow::Result<()>>;\n}", 0, "anyhow_pub|crates/demo/src/lib.rs|2|fixture");
});
test("anyhow_pub: an Option of the alias counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> Option<anyhow::Result<()>> { None }", 0, "anyhow_pub|crates/demo/src/lib.rs|1|fixture");
});
test("anyhow_pub: a pub trait inside an inline module counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub mod m {\n    pub trait T {\n        fn d(&self) -> anyhow::Result<()>;\n    }\n}", 0, "anyhow_pub|crates/demo/src/lib.rs|1|fixture");
});
test("anyhow_pub: an extern C fn counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub extern \"C\" fn f() -> anyhow::Result<()> { Ok(()) }", 0, "anyhow_pub|crates/demo/src/lib.rs|1|fixture");
});
test("anyhow_pub: a use-as alias counts through its fns", () => {
  expectExit("crates/demo/src/lib.rs", "use anyhow::Result as AResult;\npub fn f() -> AResult<()> { Ok(()) }\npub fn g() -> AResult<u8> { Ok(1) }", 0, "anyhow_pub|crates/demo/src/lib.rs|2|fixture");
});
test("anyhow_pub: a type alias of the alias counts through its fns", () => {
  expectExit("crates/demo/src/lib.rs", "pub type Res<T> = anyhow::Result<T>;\npub fn f() -> Res<()> { Ok(()) }", 0, "anyhow_pub|crates/demo/src/lib.rs|1|fixture");
});
test("anyhow_pub: a macro body that names its fn counts", () => {
  expectExit("crates/demo/src/lib.rs", "macro_rules! m {\n    ($name:ident) => {\n        pub fn $name() -> anyhow::Result<()> { Ok(()) }\n    };\n}", 0, "anyhow_pub|crates/demo/src/lib.rs|1|fixture");
});

// Batch 4 prelude (b): string literals are blanked, and `use anyhow as X` names a module alias.
test("anyhow_pub: a string that holds the signature does not count", () => {
  expectExit("crates/demo/src/lib.rs", "pub const S: &str = \"pub fn f() -> anyhow::Result<()>\";", 0);
});
test("anyhow_pub: a module alias of anyhow counts through its fns", () => {
  expectExit("crates/demo/src/lib.rs", "use anyhow as ah;\npub fn f() -> ah::Result<()> { Ok(()) }", 0, "anyhow_pub|crates/demo/src/lib.rs|1|fixture");
});

// The manual_eq_ord rule: a hand-written Eq, Ord or PartialOrd needs a row and a reason.
const manualEq = "pub struct Key(u8);\nimpl Eq for Key {}";
test("manual_eq_ord: a manual Eq impl with no row fails", () => {
  expectExit("crates/demo/src/lib.rs", manualEq, 1, "");
});
test("manual_eq_ord: the exact count is allowed", () => {
  expectExit("crates/demo/src/lib.rs", manualEq, 0, "manual_eq_ord|crates/demo/src/lib.rs|1|fixture");
});
test("manual_eq_ord: a derive does not count", () => {
  expectExit("crates/demo/src/lib.rs", "#[derive(PartialEq, Eq)]\npub struct Key(u8);", 0, "");
});
test("manual_eq_ord: an Ord and a PartialOrd impl count separately", () => {
  expectExit("crates/demo/src/lib.rs", "impl Ord for K {}\nimpl PartialOrd for K {}", 0, "manual_eq_ord|crates/demo/src/lib.rs|2|fixture");
});

// The manual_hash rule: a hand-written Hash must agree with Eq, so it needs a row and a reason.
test("manual_hash: a manual Hash impl with no row fails", () => {
  expectExit("crates/demo/src/lib.rs", "pub struct Key(u8);\nimpl std::hash::Hash for Key {\n    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {\n        self.0.hash(state);\n    }\n}", 1, "");
});
test("manual_hash: a derive does not count", () => {
  expectExit("crates/demo/src/lib.rs", "#[derive(Hash)]\npub struct Key(u8);", 0, "");
});

// The assert_no_message rule (P13): an assert with a condition or values and no message needs a row.
test("assert_no_message: an assert! with no message fails with no row", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(x: bool) {\n    assert!(x);\n}", 1, "");
});
test("assert_no_message: an assert! with a message passes", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(x: bool) {\n    assert!(x, \"the flag is set\");\n}", 0, "");
});
test("assert_no_message: an assert_eq! with two values and no message fails", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(a: u8) {\n    assert_eq!(a, 1);\n}", 1, "");
});
test("assert_no_message: an assert_eq! with a message passes, across lines", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(a: u8) {\n    assert_eq!(\n        a,\n        1,\n        \"a is {a}\"\n    );\n}", 0, "");
});
test("assert_no_message: a // inside a string literal is not a comment", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(url: &str) {\n    assert_eq!(url, \"http://host\", \"the url\");\n}", 0, "");
});
test("assert_no_message: a quote inside a raw string does not end the string", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(a: &str) {\n    assert_eq!(a, r#\"x\"y\"#, \"the text\");\n}", 0, "");
});
test("assert_no_message: a double-quote char literal does not open a string", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(a: char) {\n    assert_eq!(a, '\"', \"the char\");\n}", 0, "");
});
test("assert_no_message: a trailing comma adds no message", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(x: bool) {\n    assert!(x,);\n}", 1, "");
});
test("assert_no_message: a debug_assert! is not an assert! here", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(x: bool) {\n    debug_assert!(x);\n}", 0, "");
});
test("assert_no_message: the exact count is allowed", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(x: bool) {\n    assert!(x);\n    assert!(!x);\n}", 0, "assert_no_message|crates/demo/src/lib.rs|2|fixture");
});

// The assert_repeats_condition rule (P14): a message that only repeats the condition fails.
test("assert_repeats_condition: a message that repeats the condition fails", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(x: bool) {\n    assert!(x, \"x\");\n}", 1, "");
});
test("assert_repeats_condition: spacing and case do not hide a repeat", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(a: u8) {\n    assert!(a == 1, \"A  ==  1\");\n}", 1, "");
});
test("assert_repeats_condition: a message that says why passes", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(a: u8) {\n    assert!(a == 1, \"the frame count is one (got {a})\");\n}", 0, "");
});
test("assert_repeats_condition: a bracket inside a message does not end the call", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(a: u8) {\n    assert!(a == 1, \"got )\"); assert!(a == 1, \"a == 1\");\n}", 1, "");
});

// The long_chain rule (P15): a statement with five or more chained calls needs a row.
const longChain = "fn f(v: Vec<u8>) -> usize {\n    v.iter().map(|x| x + 1).filter(|x| *x > 1).count()\n}";
test("long_chain: a chain of five calls fails with no row", () => {
  expectExit("crates/demo/src/lib.rs", longChain.replace("count()", "take(3).count()"), 1, "");
});
test("long_chain: a chain of four calls passes", () => {
  expectExit("crates/demo/src/lib.rs", longChain, 0, "");
});
test("long_chain: the exact count is allowed", () => {
  expectExit("crates/demo/src/lib.rs", longChain.replace("count()", "take(3).count()"), 0, "long_chain|crates/demo/src/lib.rs|1|fixture");
});

// The untyped_generic_let rule (P4): a let that takes its type from parse, into or try_into needs a row.
test("untyped_generic_let: an untyped parse with no row fails", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(s: &str) -> Result<u8, std::num::ParseIntError> {\n    let n = s.parse()?;\n    Ok(n)\n}", 1, "");
});
test("untyped_generic_let: a typed let passes", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(s: &str) -> Result<u8, std::num::ParseIntError> {\n    let n: u8 = s.parse()?;\n    Ok(n)\n}", 0, "");
});
test("untyped_generic_let: the exact count is allowed", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(s: &str) -> Result<u8, std::num::ParseIntError> {\n    let n = s.parse()?;\n    Ok(n)\n}", 0, "untyped_generic_let|crates/demo/src/lib.rs|1|fixture");
});

// The turbofish_collect_parse rule (P2): a collect or parse turbofish needs a row.
test("turbofish_collect_parse: a collect turbofish with no row fails", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(v: &[u8]) -> Vec<u8> {\n    v.iter().copied().collect::<Vec<u8>>()\n}", 1, "");
});
test("turbofish_collect_parse: a typed let passes", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(v: &[u8]) -> Vec<u8> {\n    let out: Vec<u8> = v.iter().copied().collect();\n    out\n}", 0, "");
});
test("turbofish_collect_parse: a forced _ passes", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(s: &str) -> Option<u8> {\n    s.parse::<_>().ok()\n}", 0, "");
});
test("dyn_param: a dyn parameter counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(sink: &dyn Sink) {}", 1);
});
test("dyn_param: a counted dyn parameter passes", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(sink: &dyn Sink) {}", 0, "dyn_param|crates/demo/src/lib.rs|1|fixture");
});
test("dyn_param: a generic parameter is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f<T: Sink>(sink: &T) {}", 0);
});
test("dyn_param: a dyn return is not a parameter", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> Box<dyn Sink> { todo!() }", 0);
});

// The untyped_long_chain_let rule (P5): an untyped let of a five-call chain needs a row.
const untypedChain = "fn f(v: &[u8]) -> usize {\n    let n = v.iter().map(|x| x + 1).filter(|x| *x > 1).take(3).count();\n    n\n}";
test("untyped_long_chain_let: an untyped let of five calls fails with no row", () => {
  expectExit("crates/demo/src/lib.rs", untypedChain, 1, "");
});
test("untyped_long_chain_let: a typed let passes", () => {
  expectExit("crates/demo/src/lib.rs", untypedChain.replace("let n =", "let n: usize ="), 0, "long_chain|crates/demo/src/lib.rs|1|fixture");
});
test("untyped_long_chain_let: the exact count is allowed", () => {
  expectExit("crates/demo/src/lib.rs", untypedChain, 0, "long_chain|crates/demo/src/lib.rs|1|fixture\nuntyped_long_chain_let|crates/demo/src/lib.rs|1|fixture");
});

// The pub_vec_return rule: a public function returns a Vec, outside tests and examples.
test("pub_vec_return: a pub fn that returns a Vec counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> Vec<u8> { Vec::new() }", 1);
});
test("pub_vec_return: a multi-line signature counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(\n    x: u8,\n) -> Vec<u8> {\n    vec![x]\n}", 1);
});
test("pub_vec_return: a Vec inside a Result counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> Result<Vec<u8>, ()> { Ok(Vec::new()) }", 1);
});
test("pub_vec_return: an iterator is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> impl Iterator<Item = u8> { std::iter::empty() }", 0);
});
test("pub_vec_return: a private fn is clean", () => {
  expectExit("crates/demo/src/lib.rs", "fn f() -> Vec<u8> { Vec::new() }", 0);
});
test("pub_vec_return: a test module counts", () => {
  expectExit("crates/demo/src/lib.rs", "#[cfg(test)]\nmod tests {\n    pub fn f() -> Vec<u8> { Vec::new() }\n}", 1);
});
test("pub_vec_return: an example counts", () => {
  expectExit("crates/demo/examples/e.rs", "pub fn f() -> Vec<u8> { Vec::new() }", 1);
});
test("pub_vec_return: an allowed row with its count passes", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> Vec<u8> { Vec::new() }", 0, "pub_vec_return|crates/demo/src/lib.rs|1|the owned bytes");
});

// The pub_vec_param rule: a public function takes a Vec parameter, outside tests and examples.
test("pub_vec_param: a pub fn with a Vec parameter counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(xs: Vec<u8>) -> usize { xs.len() }", 1);
});
test("pub_vec_param: a multi-line parameter list counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(\n    a: u8,\n    xs: Vec<u8>,\n) -> usize {\n    xs.len()\n}", 1);
});
test("pub_vec_param: an impl IntoIterator is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(xs: impl IntoIterator<Item = u8>) -> usize { xs.into_iter().count() }", 0);
});
test("pub_vec_param: a private fn is clean", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(xs: Vec<u8>) -> usize { xs.len() }", 0);
});
test("pub_vec_param: an allowed row with its count passes", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(xs: Vec<u8>) -> usize { xs.len() }", 0, "pub_vec_param|crates/demo/src/lib.rs|1|keeps the list");
});

// The pub_impl_into rule: a public function takes impl Into<T>, outside tests and examples.
test("pub_impl_into: a pub fn with impl Into counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(s: impl Into<String>) -> String { s.into() }", 1);
});
test("pub_impl_into: a private fn is clean", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(s: impl Into<String>) -> String { s.into() }", 0);
});
test("pub_impl_into: an allowed row with its count passes", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(s: impl Into<String>) -> String { s.into() }", 0, "pub_impl_into|crates/demo/src/lib.rs|1|a validating constructor");
});

// The app_impl_into rule: application code (main.rs and bin/ targets) takes a concrete type.
test("app_impl_into: a main.rs fn with impl Into counts", () => {
  expectExit("crates/demo/src/main.rs", "fn f(s: impl Into<String>) -> String { s.into() }", 1);
});
test("app_impl_into: a bin target counts", () => {
  expectExit("crates/demo/src/bin/tool.rs", "fn f(s: impl Into<String>) -> String { s.into() }", 1);
});
test("app_impl_into: a library file is clean", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(s: impl Into<String>) -> String { s.into() }", 0);
});

// The accessor_name rule: the names and receivers of the public accessors (U9 to U14).
const accessor = (body: string) => `impl Demo {\n${body}\n}`;
test("accessor_name: a get_ method counts", () => {
  expectExit("crates/demo/src/lib.rs", accessor("    pub fn get_x(&self) -> u8 {\n        1\n    }"), 1);
});
test("accessor_name: a replace_ method with no return counts", () => {
  expectExit("crates/demo/src/lib.rs", accessor("    pub fn replace_x(&mut self, v: u8) {\n        let _ = v;\n    }"), 1);
});
test("accessor_name: a take_ method that does not take &mut self counts", () => {
  expectExit("crates/demo/src/lib.rs", accessor("    pub fn take_x(self) -> u8 {\n        1\n    }"), 1);
});
test("accessor_name: a _mut method that returns a shared ref counts", () => {
  expectExit("crates/demo/src/lib.rs", accessor("    pub fn x_mut(&mut self) -> &u8 {\n        &self.v\n    }"), 1);
});
test("accessor_name: a set_ method that takes a reference counts", () => {
  expectExit("crates/demo/src/lib.rs", accessor("    pub fn set_x(&mut self, v: &u8) {\n        let _ = v;\n    }"), 1);
});
test("accessor_name: the guide's shapes are clean", () => {
  expectExit("crates/demo/src/lib.rs", accessor("    pub fn x(&self) -> &u8 {\n        &self.v\n    }\n    pub fn x_mut(&mut self) -> &mut u8 {\n        &mut self.v\n    }\n    pub fn take_x(&mut self) -> u8 {\n        std::mem::take(&mut self.v)\n    }\n    pub fn set_x(&mut self, v: u8) {\n        self.v = v;\n    }"), 0);
});
test("accessor_name: a free take_ function is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn take_array(bytes: &[u8], cursor: &mut usize) -> u8 {\n    let _ = (bytes, cursor);\n    0\n}", 0);
});

// The bool_param_comment rule: a public function's bool parameter has its purpose written above it (I14).
test("bool_param_comment: a bool with no comment counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(on: bool) {\n    let _ = on;\n}", 1);
});
test("bool_param_comment: a bool in a multi-line list with no comment counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(\n    on: bool,\n) {\n    let _ = on;\n}", 1);
});
test("bool_param_comment: a comment above the bool is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(\n    // true turns it on\n    on: bool,\n) {\n    let _ = on;\n}", 0);
});
test("bool_param_comment: a private fn is clean", () => {
  expectExit("crates/demo/src/lib.rs", "fn f(on: bool) {\n    let _ = on;\n}", 0);
});


// The restating_doc rule (D7): a doc comment that only names its item again fails.
test("restating_doc: a doc that restates the name fails with no row", () => {
  expectExit("crates/demo/src/lib.rs", "/// Returns the foo.\npub fn foo() {}", 1, "");
});
test("restating_doc: a doc that says more than the name passes", () => {
  expectExit("crates/demo/src/lib.rs", "/// The size of the window, in bytes.\npub fn foo() {}", 0, "");
});
test("restating_doc: a doc with an attribute between it and the item still counts", () => {
  expectExit("crates/demo/src/lib.rs", "/// Sets the foo.\n#[must_use]\npub fn foo() {}", 1, "");
});

// The doc_basics rule (D6): a doc comment that explains Rust itself fails.
test("doc_basics: this is a function in a doc fails with no row", () => {
  expectExit("crates/demo/src/lib.rs", "/// This is a function that parses the frame.\npub fn parse() {}", 1, "");
});
test("doc_basics: a doc that says what the item does passes", () => {
  expectExit("crates/demo/src/lib.rs", "/// Parses the frame, or refuses a short one.\npub fn parse() {}", 0, "");
});

// The self_convention rule: as_ and to_ take a reference, into_ takes self (N1).
const selfConv = (body: string) => `impl Demo {\n${body}\n}`;
test("self_convention: a by-value as_ counts", () => {
  expectExit("crates/demo/src/lib.rs", selfConv("    pub fn as_x(self) -> u8 {\n        1\n    }"), 1);
});
test("self_convention: a by-value to_ counts", () => {
  expectExit("crates/demo/src/lib.rs", selfConv("    pub fn to_x(self) -> u8 {\n        1\n    }"), 1);
});
test("self_convention: an into_ by reference counts", () => {
  expectExit("crates/demo/src/lib.rs", selfConv("    pub fn into_x(&self) -> u8 {\n        1\n    }"), 1);
});
test("self_convention: the guide's shapes are clean", () => {
  expectExit("crates/demo/src/lib.rs", selfConv("    pub fn as_x(&self) -> u8 {\n        1\n    }\n    pub fn to_x(&self) -> u8 {\n        1\n    }\n    pub fn into_x(self) -> u8 {\n        1\n    }"), 0);
});

// The generic_name rule: a generic parameter is a short letter from the allowed set, or a CamelCase word (N7).
test("generic_name: a single letter outside the set counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub struct Wrap<A>(pub A);", 1);
});
test("generic_name: a lowercase generic counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f<x: Copy>(v: x) -> x {\n    v\n}", 1);
});
test("generic_name: the allowed letters and a CamelCase word are clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub struct Pair<T, Handle>(pub T, pub Handle);\npub fn f<Decoder: Copy>(v: Decoder) -> Decoder {\n    v\n}", 0);
});

// The fn_import rule (O23): a top-level function imported by its bare name needs a row.
test("fn_import: a bare function import with no row fails", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn build_mesh() {}\nuse crate::demo::build_mesh;", 1, "");
});
test("fn_import: a module import is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub mod wire {}\nuse crate::demo::wire;", 0, "");
});
test("fn_import: the exact count is allowed", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn build_mesh() {}\nuse crate::demo::build_mesh;", 0, "fn_import|crates/demo/src/lib.rs|1|fixture");
});

// The abbrev_name rule: an identifier that is a letter-deleted abbreviation counts, a string or a comment does not (N13).
test("abbrev_name: a local named ctx counts", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> u8 {\n    let ctx = 1;\n    ctx\n}", 1);
});
test("abbrev_name: the word in a string or a comment is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() -> &'static str {\n    // ctx is a comment\n    \"ctx\"\n}", 0);
});
test("abbrev_name: a path such as rand::rng is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() {\n    let _ = rand::rng();\n}", 0);
});

// The trait_import_scope rule (O22): a top-level `use ... as _;` needs a row.
test("trait_import_scope: a top-level as _ import with no row fails", () => {
  expectExit("crates/demo/src/lib.rs", "use std::io::Write as _;\npub fn f() {}", 1, "");
});
test("trait_import_scope: an indented as _ import inside a fn passes", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f() {\n    use std::io::Write as _;\n}", 0, "");
});

// Tests, benches and examples are in scope of the public-API rules: no path is skipped but a binary.
test("anyhow_pub: a file under tests counts", () => {
  expectExit("crates/demo/tests/t.rs", "pub fn f() -> anyhow::Result<()> { Ok(()) }", 1);
});
test("dyn_param: a file under benches counts", () => {
  expectExit("crates/demo/benches/b.rs", "pub fn f(x: &dyn std::fmt::Debug) {}", 1);
});
test("pub_vec_param: a file under tests counts", () => {
  expectExit("crates/demo/tests/t.rs", "pub fn f(xs: Vec<u8>) -> usize { xs.len() }", 1);
});

// The prelude_import rule (O19): a named import from a prelude fails, the wasm_bindgen macro is the one exception.
test("prelude_import: a named import from a prelude fails", () => {
  expectExit("crates/demo/src/lib.rs", "use proptest::prelude::any;", 1);
});
test("prelude_import: a grouped import from a prelude fails", () => {
  expectExit("crates/demo/src/lib.rs", "use wasm_bindgen::prelude::{JsValue, wasm_bindgen};", 1);
});
test("prelude_import: a std prelude import fails", () => {
  expectExit("crates/demo/src/lib.rs", "use std::prelude::v1::Option;", 1);
});
test("prelude_import: the wasm_bindgen macro is allowed", () => {
  expectExit("crates/demo/src/lib.rs", "use wasm_bindgen::prelude::wasm_bindgen;", 0);
});
test("prelude_import: the word in a comment or a path with no prelude is clean", () => {
  expectExit("crates/demo/src/lib.rs", "// use proptest::prelude::any;\nuse proptest::arbitrary::any;", 0);
});

// The str_replace rule (P23): str::replace and str::replacen fail, a one-argument replace is another type.
test("str_replace: a str replace with two arguments fails", () => {
  expectExit("crates/demo/src/lib.rs", 'pub fn f(s: &str) -> String {\n    s.replace("a", "b")\n}', 1);
});
test("str_replace: a replacen fails", () => {
  expectExit("crates/demo/src/lib.rs", 'pub fn f(s: &str) -> String {\n    s.replacen(\'a\', "b", 1)\n}', 1);
});
test("str_replace: a chain split across lines fails", () => {
  expectExit("crates/demo/src/lib.rs", 'pub fn f(s: &str) -> String {\n    s\n        .replace("a,", "b")\n}', 1);
});
test("str_replace: a replace with one argument is clean", () => {
  expectExit("crates/demo/src/lib.rs", "pub fn f(slot: &mut Option<u8>) -> Option<u8> {\n    slot.replace(1)\n}", 0);
});
test("str_replace: the word in a string or a comment is clean", () => {
  expectExit("crates/demo/src/lib.rs", 'pub fn f() -> &\'static str {\n    // s.replace("a", "b")\n    ".replace(a, b)"\n}', 0);
});
test("str_replace: a test file counts too", () => {
  expectExit("crates/demo/tests/t.rs", '#[test]\nfn t() {\n    let _ = "a".replace(\'a\', "b");\n}', 1);
});
