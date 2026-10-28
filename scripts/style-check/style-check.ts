#!/usr/bin/env bun
// Rules of the Google Rust Style Guide (issue #29) that no compiler lint checks.
// Each rule is one pattern. A match fails the check unless `style-check.allow` names the file with
// at least that many matching lines. A new match in an allowed file raises the count, so it fails
// too. A count that is higher than the matches fails as well: lower it when you remove an exception.
// `style-check.test.ts` plants a violation for each rule and for each way the allow list can break.

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const allowFile = process.env.STYLE_CHECK_ALLOW || "scripts/style-check/style-check.allow";

// A rule is a per-line `pattern`, or a whole-file `scan` for a form that rustfmt can split across lines.
// `skip` leaves a file out of the count.
type Rule = {
  name: string;
  pattern?: RegExp;
  scan?: (text: string) => number;
  // A count from the file path alone, for a rule that reads other files of the crate.
  scanFile?: (file: string) => number;
  skip?: RegExp;
  glob: string;
  why: string;
};

// The public signatures that return anyhow. Comments and `#[cfg(test)] mod` bodies are cut
// first. A `pub fn` (also `pub extern "C" fn`, and a `$name` in a macro body) counts when its
// return type names `anyhow::Result` or `anyhow::Error` anywhere, or a bare one-argument
// `Result<T>` in a file that imports anyhow's `Result`, or an alias of either. A method of a
// `pub trait` counts the same way, at any indent. A `FromStr` or `TryFrom` impl with anyhow as
// its error counts once.
const ANYHOW_ERR = /\btype (?:Err|Error)\s*=\s*(?:::)?anyhow::Error\b/g;
const PUB_FN = /\bpub (?:const |async |unsafe |extern(?: "")? )*fn (\$?\w+)/g;
const TRAIT_FN = /\bfn (\$?\w+)/g;
const PUB_TRAIT = /\bpub trait\b[^{;]*\{/g;
const ANYHOW_USE_AS = /\buse (?:::)?anyhow::Result as (\w+)\s*;/g;
const ANYHOW_USE_BARE = /^\s*use (?:::)?anyhow::\{?[^;]*\bResult\b(?!\s+as\b)[^;]*;/m;
const ANYHOW_MODULE_ALIAS = /\buse (?:::)?anyhow as (\w+)\s*;/g;
const ANYHOW_TYPE_ALIAS = /\btype (\w+)(?:<[^=]*>)?\s*=\s*(?:::)?anyhow::Result\b/g;

// The lexemes in which a `//`, a `/*` or a `"` is not what it looks like: a raw string, a string, a
// char literal (`'"'`), then the two comment forms. Matched in this order, left to right, so a `//`
// inside a string stays text and a quote inside a raw string does not end it.
const LEXEME = /b?r(#*)"[\s\S]*?"\1|b?"(?:[^"\\]|\\[\s\S])*"|'(?:[^'\\\n]|\\.[^'\n]*)'|\/\*[\s\S]*?\*\/|\/\/[^\n]*/g;

/** Text with line comments and block comments blanked, so a commented-out signature counts nothing. */
function stripComments(text: string): string {
  return text.replace(LEXEME, (lexeme) => {
    if (lexeme.startsWith("/*")) return " ";
    if (lexeme.startsWith("//")) return "";
    return lexeme;
  });
}

/** Text with the body of each string literal blanked, so a string that holds a signature counts nothing. */
function blankStrings(text: string): string {
  return text.replace(LEXEME, (lexeme) => {
    if (lexeme.startsWith("/*") || lexeme.startsWith("//")) return lexeme;
    return /^b?r?#*"/.test(lexeme) ? "\"\"" : lexeme;
  });
}

/** The index of the `}` that closes the `{` at `open`. */
function closeBrace(text: string, open: number): number {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    if (text[i] === "{") depth++;
    else if (text[i] === "}" && --depth === 0) return i;
  }
  return text.length - 1;
}

/** `text` with the body of every `#[cfg(test)] mod name { ... }` removed, and nothing else. */
function stripTestModules(text: string): string {
  const pattern = /#\[cfg\(test\)\]\s*\n\s*mod \w+\s*\{/;
  let out = text;
  for (let m = pattern.exec(out); m; m = pattern.exec(out)) {
    const open = m.index + m[0].length - 1;
    out = out.slice(0, m.index) + out.slice(closeBrace(out, open) + 1);
  }
  return out;
}

/** The return type after the parameter list at `from`: up to `{`, `;` or `where`. */
function returnType(text: string, from: number): string {
  let depth = 0;
  let i = from;
  for (; i < text.length; i++) {
    if (text[i] === "(") depth++;
    else if (text[i] === ")" && --depth === 0) break;
  }
  const arrow = /^\s*->/.exec(text.slice(i + 1));
  if (!arrow) return "";
  const rest = text.slice(i + 1 + arrow[0].length);
  const end = rest.search(/[{;]|\bwhere\b/);
  return (end < 0 ? rest : rest.slice(0, end)).trim();
}

/** Whether `ret` has `name` with exactly one top-level type argument, as in `Result<T>`. */
function isBareOneArg(ret: string, name: string): boolean {
  const at = new RegExp(`(?:^|[^\\w:])${name}<`).exec(ret);
  if (!at) return false;
  let angle = 0;
  let nested = 0;
  let commas = 0;
  for (let i = at.index + at[0].length - 1; i < ret.length; i++) {
    const c = ret[i];
    if (c === "<") angle++;
    else if (c === ">" && --angle === 0) return commas === 0;
    else if (c === "(" || c === "[") nested++;
    else if (c === ")" || c === "]") nested--;
    else if (c === "," && angle === 1 && nested === 0) commas++;
  }
  return false;
}

// The closing bracket of the `(` at `open`, skipping nested brackets. Strings are blanked first.
function closeParen(text: string, open: number): number {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    const ch = text[i] ?? "";
    if ("([{".includes(ch)) depth++;
    else if (")]}".includes(ch) && --depth === 0) return i;
  }
  return text.length - 1;
}

// The number of arguments of a macro call, from the text between its brackets. A trailing comma adds none.
function argCount(args: string): number {
  let depth = 0;
  let commas = 0;
  for (const ch of args) {
    if ("([{".includes(ch)) depth++;
    else if (")]}".includes(ch)) depth--;
    else if (ch === "," && depth === 0) commas++;
  }
  const last = args.slice(args.lastIndexOf(",") + 1).trim();
  if (args.trim() === "") return 0;
  return last === "" ? commas : commas + 1;
}

// The asserts with no message: assert! needs a condition and a message, assert_eq! and assert_ne! need
// two values and a message. A macro with a message already counts as one argument more.
function countAssertsNoMessage(raw: string): number {
  const text = blankStrings(stripComments(raw));
  const pattern = /\bassert(_eq|_ne)?!\s*\(/g;
  let count = 0;
  for (let m = pattern.exec(text); m; m = pattern.exec(text)) {
    const open = m.index + m[0].length - 1;
    const args = text.slice(open + 1, closeParen(text, open));
    if (argCount(args) < (m[1] ? 3 : 2)) count++;
  }
  return count;
}

// The index of the bracket that closes the `(` at `open`. A string literal is skipped whole, so a
// bracket inside a message does not close the call.
function closeCall(text: string, open: number): number {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    const ch = text[i] ?? "";
    if (ch === '"') {
      for (i++; i < text.length && text[i] !== '"'; i++) if (text[i] === "\\") i++;
    } else if ("([{".includes(ch)) depth++;
    else if (")]}".includes(ch) && --depth === 0) return i;
  }
  return text.length - 1;
}

// The top-level arguments of a macro call, trimmed. A string literal is one piece of its argument.
function callArgs(args: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let cur = "";
  for (let i = 0; i < args.length; i++) {
    const ch = args[i] ?? "";
    if (ch === '"') {
      const start = i;
      for (i++; i < args.length && args[i] !== '"'; i++) if (args[i] === "\\") i++;
      cur += args.slice(start, i + 1);
      continue;
    }
    if ("([{".includes(ch)) depth++;
    else if (")]}".includes(ch)) depth--;
    if (ch === "," && depth === 0) {
      out.push(cur.trim());
      cur = "";
    } else cur += ch;
  }
  if (cur.trim() !== "") out.push(cur.trim());
  return out;
}

// The asserts whose message only repeats the condition: `assert!(x, "x")`. Letters and digits are
// compared, so spacing and case do not hide a copy.
function countAssertsRepeatingCondition(raw: string): number {
  const text = stripComments(raw);
  const pattern = /\bassert!\s*\(/g;
  const key = (s: string) => s.replace(/[^A-Za-z0-9]/g, "").toLowerCase();
  let count = 0;
  for (let m = pattern.exec(text); m; m = pattern.exec(text)) {
    const open = m.index + m[0].length - 1;
    const args = callArgs(text.slice(open + 1, closeCall(text, open)));
    const message = args[1]?.match(/^"(.*)"$/s)?.[1];
    if (args.length === 2 && message !== undefined && key(message) === key(args[0] ?? "")) count++;
  }
  return count;
}

// The statements with a method chain of five or more calls. A statement ends at a `;`, a `{` or a `}`
// outside a string, so a chain cannot hide across a block. Comments and strings are blanked first.
const CHAIN_LIMIT = 5;
function countLongChains(raw: string): number {
  const text = blankStrings(stripComments(raw));
  let count = 0;
  for (const stmt of text.split(/[;{}]/)) {
    const calls = stmt.match(/\.\s*\w+\s*(?:::<[^>]*>\s*)?\(/g);
    if (calls && calls.length >= CHAIN_LIMIT) count++;
  }
  return count;
}

// The `let` bindings with no type whose value is a generic-return call that ends the statement:
// `let x = s.parse()?;`, `let x = v.into();`. The reader sees no type in the code, so the type is named.
function countUntypedGenericLets(raw: string): number {
  const text = blankStrings(stripComments(raw));
  let count = 0;
  for (const m of text.matchAll(/\blet\s+(?:mut\s+)?\w+\s*=\s*([^;]*);/g)) {
    if (/\.(?:parse|into|try_into)\(\)(?:\?|\.expect\([^()]*\)|\.unwrap\(\))?\s*$/.test(m[1] ?? "")) count++;
  }
  return count;
}

// The turbofish of collect and parse: `.collect::<T>()`, `.parse::<T>()`. A typed `let` names the type
// on the variable instead, so the reader finds it at the binding (P2). A `_` the compiler forces stays.
function countTurbofish(raw: string): number {
  const text = blankStrings(stripComments(raw));
  return text.match(/\.(?:collect|parse)::<(?!_>)[^;]*?>\(/g)?.length ?? 0;
}

// The lets with no type whose value is a method chain of five or more calls. The reader must infer
// the type of the result from the whole chain, so the let names it (P5). A typed let is not counted.
function countUntypedLongChainLets(raw: string): number {
  const text = blankStrings(stripComments(raw));
  let count = 0;
  for (const m of text.matchAll(/\blet\s+(?:mut\s+)?\w+\s*=\s*([^;]*);/g)) {
    const calls = (m[1] ?? "").match(/\.\s*\w+\s*(?:::<[^>]*>\s*)?\(/g);
    if (calls && calls.length >= CHAIN_LIMIT) count++;
  }
  return count;
}

// The accessor names of the guide's Rust rules U9 to U14, on the methods of a public type. A method
// is indented, so a free function that takes a cursor, such as take_array, is not one.
function countAccessorNames(raw: string): number {
  const text = stripTestModules(blankStrings(stripComments(raw)));
  let count = 0;
  for (const m of text.matchAll(/^([ \t]*)pub (?:const )?fn (\w+)/gm)) {
    const [, indent = "", name = ""] = m;
    const method = indent.length >= 4;
    const after = m.index! + m[0].length;
    const open = text.indexOf("(", after);
    const ret = returnType(text, after);
    const params = text.slice(open + 1, open + 1 + 200);
    if (name.startsWith("get_")) count++;
    if (name.startsWith("replace_") && ret === "") count++;
    if (method && name.startsWith("take_") && !/^\s*&mut self/.test(params)) count++;
    if (method && name.endsWith("_mut") && !ret.includes("&mut")) count++;
    if (method && name.startsWith("set_") && /^\s*&(?:mut )?self,\s*\w+:\s*&/.test(params)) count++;
  }
  return count;
}

// A public fn whose bool parameter has no comment above it. The guide asks for the purpose of a bool
// parameter to be written beside it, so a call site says what true means.
function countBoolParamNoComment(raw: string): number {
  const text = stripTestModules(blankStrings(raw));
  let count = 0;
  for (const m of text.matchAll(PUB_FN)) {
    const open = text.indexOf("(", m.index! + m[0].length);
    let depth = 0;
    let close = open;
    for (; close < text.length; close++) {
      if (text[close] === "(") depth++;
      else if (text[close] === ")" && --depth === 0) break;
    }
    const block = text.slice(open + 1, close);
    if (!block.includes("\n")) {
      count += [...block.matchAll(/:\s*bool\b/g)].length;
      continue;
    }
    const lines = block.split("\n");
    lines.forEach((line, i) => {
      if (!/:\s*bool\b/.test(line)) return;
      let j = i - 1;
      while (j >= 0 && lines[j]!.trim() === "") j--;
      if (!(j >= 0 && lines[j]!.trim().startsWith("//"))) count++;
    });
  }
  return count;
}

// The receiver of a public as_, to_ or into_ method. The guide has as_ and to_ take a reference, and into_
// take self. A by-value as_ or to_ is allowed on a Copy type, so each one has a counted row.
function countSelfConvention(raw: string): number {
  const text = stripTestModules(blankStrings(stripComments(raw)));
  let count = 0;
  for (const m of text.matchAll(/^[ \t]*pub (?:const )?fn (as|to|into)_\w+\s*\(\s*(&\s*(?:mut\s+)?)?self\b/gm)) {
    const [, kind, ref] = m;
    if (kind === "into" && ref !== undefined) count++;
    if ((kind === "as" || kind === "to") && ref === undefined) count++;
  }
  return count;
}

// The generic parameters of a declaration that are not T, U, E, K, V, F, I, R or S, and not a CamelCase word of
// three or more letters. A single letter says nothing about what the parameter stands for.
function countGenericNames(raw: string): number {
  const text = stripTestModules(blankStrings(raw.replace(/\/\/[^\n]*/g, "")));
  const allowed = /^(?:[TUEKVFIRS]|[A-Z][A-Za-z]{2,})$/;
  let count = 0;
  const decls = [...text.matchAll(/\b(?:fn|struct|enum|trait|type|union) \w+<([^>{;()]*)>/g), ...text.matchAll(/^\s*impl<([^>{;()]*)>/gm)];
  for (const m of decls) {
    for (const part of (m[1] ?? "").split(",")) {
      const name = part.trim().split(/[:\s=]/)[0] ?? "";
      if (!name || name.startsWith("'") || name === "const") continue;
      if (!allowed.test(name)) count++;
    }
  }
  return count;
}

// The letter-deleted abbreviations of identifiers (N13). A word in a comment, a string or an attribute
// does not count, and neither does a path such as `rand::rng`.
const ABBREVIATIONS = new Set(["ctx", "addr", "conn", "msg", "buf", "opts", "prev", "req", "resp", "idx", "cfg", "tmp", "attr", "cmd", "ident", "rng", "pos", "sig"]);
function countAbbrevNames(raw: string): number {
  const text = stripTestModules(blankStrings(stripComments(raw)))
    .split("\n")
    .filter((line) => !/^\s*#\[/.test(line))
    .join("\n");
  let count = 0;
  for (const m of text.matchAll(/(?<!::)(?<!:)\b([A-Za-z_]\w*)\b/g)) {
    if (ABBREVIATIONS.has(m[1]!)) count++;
  }
  return count;
}

// A public fn that returns a Vec. The guide prefers an iterator, so a Vec needs a counted allow row with its reason.
function countPubVecReturn(raw: string): number {
  const text = stripTestModules(blankStrings(stripComments(raw)));
  let count = 0;
  for (const m of text.matchAll(PUB_FN)) {
    if (/\bVec\s*</.test(returnType(text, m.index! + m[0].length))) count++;
  }
  return count;
}

// A public fn with a Vec parameter. The guide prefers impl IntoIterator when the fn only iterates; a fn that keeps the list needs the Vec.
function countPubVecParam(raw: string): number {
  const text = stripTestModules(blankStrings(stripComments(raw)));
  let count = 0;
  for (const m of text.matchAll(PUB_FN)) {
    const open = text.indexOf("(", m.index! + m[0].length);
    let depth = 0;
    let i = open;
    for (; i < text.length; i++) {
      if (text[i] === "(") depth++;
      else if (text[i] === ")" && --depth === 0) break;
    }
    if (/:\s*Vec\s*</.test(text.slice(open + 1, i))) count++;
  }
  return count;
}

// A public fn with an `impl Into<...>` parameter. The guide allows it in a library when call-site ergonomics matter most.
function countPubImplInto(raw: string): number {
  const text = stripTestModules(blankStrings(stripComments(raw)));
  let count = 0;
  for (const m of text.matchAll(PUB_FN)) {
    const open = text.indexOf("(", m.index! + m[0].length);
    let depth = 0;
    let i = open;
    for (; i < text.length; i++) {
      if (text[i] === "(") depth++;
      else if (text[i] === ")" && --depth === 0) break;
    }
    if (/\bimpl Into</.test(text.slice(open + 1, i))) count++;
  }
  return count;
}

// A library crate whose source holds no doc example: a fenced code block in a `///` or `//!` comment.
// Such a crate counts 1 in its lib.rs, so it needs a row with the reason (D2).
function countCrateWithoutExample(file: string): number {
  if (!/^crates\/[^/]+\/src\/lib\.rs$/.test(file)) return 0;
  const proc = spawnSync("git", ["grep", "-l", "--untracked", "-E", "^\\s*(///|//!).*```", "--", path.dirname(file)], { cwd: ROOT, encoding: "utf8" });
  return (proc.stdout ?? "").trim() === "" ? 1 : 0;
}

// The doc comments that only restate the name of their item: every word of the comment is a word of
// the identifier or a stopword, as in `/// Returns the foo.` on `fn foo`. A comment that adds a word
// about the value, the error or the reason is not counted (D7).
const RESTATE_STOP = new Set(["the", "a", "an", "of", "to", "for", "returns", "return", "get", "gets", "set", "sets", "new", "creates", "create"]);
function identWords(name: string): string[] {
  return name.replace(/([a-z0-9])([A-Z])/g, "$1 $2").split(/[_\s]+/).map((w) => w.toLowerCase()).filter(Boolean);
}
function countRestatingDocs(text: string): number {
  const lines = text.split("\n");
  let count = 0;
  let doc: string[] = [];
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed.startsWith("///")) {
      doc.push(trimmed.slice(3));
      continue;
    }
    if (trimmed.startsWith("#[") || trimmed === "") {
      if (trimmed === "") doc = [];
      continue;
    }
    const item = trimmed.match(/^(?:pub(?:\([^)]*\))?\s+)?(?:const\s+|async\s+|unsafe\s+)*(?:fn|struct|enum|trait|type|const|static)\s+(\w+)/);
    if (item && doc.length > 0) {
      const words = doc.join(" ").toLowerCase().match(/[a-z0-9]+/g) ?? [];
      const own = new Set([...identWords(item[1] ?? ""), ...RESTATE_STOP]);
      if (words.length > 0 && words.every((w) => own.has(w))) count++;
    }
    doc = [];
  }
  return count;
}

// The function names and the module names of the repo, read once from git grep. A lowercase name that
// is imported on its own, and is a function here (or a std function), is a top-level function that a
// call should qualify with its module (O23).
let fnAndModNames: { fns: Set<string>; mods: Set<string> } | undefined;
const NOT_FNS = new Set(["test", "mpsc", "presets", "fmt", "oneshot", "memory", "io", "catch_panic", "atomic", "webrtc", "test_relay", "tuning", "watch", "thread", "fs", "path", "time", "env", "net", "sync", "mem", "ptr", "iter", "hash", "error", "collections", "cmp", "any", "ops", "str", "vec", "process", "convert", "borrow", "marker", "num", "future", "task", "stream", "signal", "timeout", "sleep_until"]);
const STD_FNS = new Set(["swap", "replace", "take", "spawn", "sleep", "channel", "identity", "min", "max", "size_of", "from_fn", "once", "repeat", "empty", "unbounded_channel", "read_to_string", "write"]);
function declaredNames(): { fns: Set<string>; mods: Set<string> } {
  if (fnAndModNames) return fnAndModNames;
  const fns = new Set<string>();
  const mods = new Set<string>();
  const proc = spawnSync("git", ["grep", "-h", "--untracked", "-E", "(^|[[:space:]])(fn|mod)[[:space:]]+[a-z_][a-z0-9_]*", "--", "crates"], { cwd: ROOT, encoding: "utf8" });
  for (const line of (proc.stdout ?? "").split("\n")) {
    for (const m of line.matchAll(/\bfn\s+([a-z_][a-z0-9_]*)/g)) fns.add(m[1] ?? "");
    for (const m of line.matchAll(/\bmod\s+([a-z_][a-z0-9_]*)/g)) mods.add(m[1] ?? "");
  }
  fnAndModNames = { fns, mods };
  return fnAndModNames;
}
function countFnImports(raw: string): number {
  const { fns, mods } = declaredNames();
  const text = stripComments(raw);
  let count = 0;
  for (const m of text.matchAll(/^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t]+(?:[A-Za-z_][A-Za-z0-9_]*::)*([a-z_][a-z0-9_]*)[ \t]*;/gm)) {
    const name = m[1] ?? "";
    if ((fns.has(name) || STD_FNS.has(name)) && !mods.has(name) && !NOT_FNS.has(name)) count++;
  }
  return count;
}

function countAnyhowPub(raw: string): number {
  const text = stripTestModules(blankStrings(stripComments(raw)));
  const bare = new Set<string>();
  if (ANYHOW_USE_BARE.test(text)) bare.add("Result");
  for (const [, name] of text.matchAll(ANYHOW_USE_AS)) if (name) bare.add(name);
  for (const [, name] of text.matchAll(ANYHOW_TYPE_ALIAS)) if (name) bare.add(name);
  const modules = [...text.matchAll(ANYHOW_MODULE_ALIAS)].flatMap(([, name]) => (name ? [name] : []));
  const anyhowRet = (ret: string): boolean =>
    /\banyhow::(?:Result|Error)\b/.test(ret) ||
    modules.some((module) => new RegExp(`\\b${module}::(?:Result|Error)\\b`).test(ret)) ||
    [...bare].some((name) => isBareOneArg(ret, name));
  let count = [...text.matchAll(ANYHOW_ERR)].length;
  for (const m of text.matchAll(PUB_FN)) {
    if (anyhowRet(returnType(text, m.index! + m[0].length))) count++;
  }
  for (const m of text.matchAll(PUB_TRAIT)) {
    const open = m.index! + m[0].length - 1;
    const body = text.slice(open, closeBrace(text, open));
    for (const f of body.matchAll(TRAIT_FN)) {
      if (anyhowRet(returnType(body, f.index! + f[0].length))) count++;
    }
  }
  return count;
}

/** The parameter list that starts at `from`, from its `(` to the matching `)`. */
function paramsOf(text: string, from: number): string {
  let depth = 0;
  let i = from;
  for (; i < text.length; i++) {
    if (text[i] === "(") depth++;
    else if (text[i] === ")" && --depth === 0) break;
  }
  return text.slice(from, i + 1);
}

/** Public fns that take a `dyn` trait object as a parameter. A generic `T: Trait` is the library form. */
function countDynParams(raw: string): number {
  const text = stripTestModules(blankStrings(stripComments(raw)));
  let count = 0;
  for (const m of text.matchAll(PUB_FN)) {
    if (/\bdyn\b/.test(paramsOf(text, m.index! + m[0].length))) count++;
  }
  return count;
}

const RULES: Rule[] = [
  {
    name: "poison",
    pattern: /PoisonError|unwrap_or_else\([|][^|]*[|][^)]*into_inner/,
    glob: "*.rs",
    why: 'Never reach inside a poisoned mutex. Use .expect("...") and let the panic spread.',
  },
  {
    name: "feature",
    pattern: /^\s*#!?\[(cfg_attr\([^\]]*, *)?feature\(/,
    glob: "*.rs",
    why: "No unstable features. The light-cycles example is the one exception. The project toolchain is a nightly, so only this rule stops a new #![feature].",
  },
  {
    name: "catch_unwind",
    pattern: /catch_unwind/,
    glob: "*.rs",
    why: "Do not call panic::catch_unwind in first-party code. A boundary with C is the exception.",
  },
  {
    name: "ctor",
    pattern: /#\[(ctor::)?ctor\]|use ctor([^_a-zA-Z0-9]|$)/,
    glob: "*.rs",
    why: "Do not run code before main().",
  },
  {
    name: "block_doc",
    pattern: /^\s*\/\*[*!]($|[^*/])/,
    glob: "*.rs",
    why: "Document with /// and //!, not with /** */ or /*! */.",
  },
  {
    name: "lifetime",
    pattern: /\b(struct|enum|trait)\s+\w+<[^>{]*'[a-z](?![A-Za-z0-9_])/,
    glob: "*.rs",
    why: "Name a type lifetime for what it borrows ('frame, not 'a). The guide allows one letter only on a fn.",
  },
  {
    name: "glob",
    pattern: /^\s*(pub(\([^)]*\))?\s+)?use\s[^;]*(::\*|\{\s*\*|,\s*\*)/,
    glob: "*.rs",
    why: "Import each name. No glob import: not `use super::*` in a test module, not a `pub use` facade, not a third-party prelude. Clippy exempts the first two, so only this rule stops them.",
  },
  {
    name: "operator",
    pattern: /^\s*impl(<[^>]*>)?\s+(std::ops::|ops::)?(Add|Sub|Mul|Div|Rem|Neg|Not|Index|IndexMut|BitAnd|BitOr|BitXor|Shl|Shr)(Assign)?\b/,
    glob: "crates/**/*.rs",
    why: "An operator overload has a math or logic meaning only (a frame advances by frames, a flag set joins flags). Any other use needs a line in style-check.allow with a reason.",
  },
  {
    name: "pub_const_fn",
    pattern: /^\s*pub(\([^)]*\))?\s+const\s+fn\b/,
    glob: "crates/**/*.rs",
    why: "Mark a public fn const only if it will stay const (a const context, a newtype accessor, or a trivial accessor). A new one needs a row in style-check.allow with that reason.",
  },
  {
    name: "manual_eq_ord",
    pattern: /^\s*(?:unsafe\s+)?impl\b.*\b(?:Eq|Ord|PartialOrd)\s+for\b/,
    glob: "*.rs",
    why: "A hand-written Eq, Ord or PartialOrd must keep the law: Eq reflexive, Ord total. The compiler checks a derive, not a manual impl, so each one needs a row and a reason.",
  },
  {
    name: "manual_hash",
    pattern: /^\s*(?:unsafe\s+)?impl\b.*\bHash\s+for\b/,
    glob: "*.rs",
    why: "A hand-written Hash must agree with Eq: equal values hash alike. The compiler does not check that for a manual impl, so each one needs a row and a reason.",
  },
  {
    name: "assert_no_message",
    scan: countAssertsNoMessage,
    glob: "crates/**/*.rs",
    why: "An assert names what it checks: a message that adds the value, or the reason, so a failure reads without the source (P13).",
  },
  {
    name: "assert_repeats_condition",
    scan: countAssertsRepeatingCondition,
    glob: "crates/**/*.rs",
    why: "An assert message says why, not what: a message that only repeats the condition adds nothing (P14).",
  },
  {
    name: "long_chain",
    scan: countLongChains,
    glob: "crates/**/*.rs",
    why: "A statement with a chain of five or more calls is hard to read in one pass: name the middle step (P15).",
  },
  {
    name: "untyped_generic_let",
    scan: countUntypedGenericLets,
    glob: "crates/**/*.rs",
    why: "A let that takes its type from parse, into or try_into names the type (P4). The reader sees no type otherwise.",
  },
  {
    name: "turbofish_collect_parse",
    scan: countTurbofish,
    glob: "crates/**/*.rs",
    why: "Name the type on the variable, not in a turbofish, for collect and parse (P2). A `_` that the compiler forces is allowed.",
  },
  {
    name: "untyped_long_chain_let",
    scan: countUntypedLongChainLets,
    glob: "crates/**/*.rs",
    why: "A let of a chain of five or more calls names its type (P5). The reader cannot infer the result from the chain alone.",
  },
  {
    name: "crate_example",
    scanFile: countCrateWithoutExample,
    glob: "crates/*/src/lib.rs",
    why: "Every library crate shows how it works: a doc example (a fenced code block in a /// or //! comment) somewhere in its source. A crate without one needs a row with the reason (D2).",
  },
  {
    name: "restating_doc",
    scan: countRestatingDocs,
    glob: "crates/**/*.rs",
    why: "A doc comment that only restates the item name adds nothing (D7). Say what the name does not say.",
  },
  {
    name: "doc_basics",
    pattern: /^\s*\/\/[\/!].*\b(?:this is an? (?:function|struct|trait|method|type|module|enum)|an? (?:struct|trait|enum) that|returns an? (?:Result|Option)\b)/i,
    glob: "*.rs",
    why: "A doc comment does not explain Rust: no `this is a function`, no `a struct that`, no `returns a Result` (D6). It says what the item does.",
  },
  {
    name: "fn_import",
    scan: countFnImports,
    glob: "crates/**/*.rs",
    why: "Call a top-level function through its module (mem::swap, not a bare swap), so the reader sees where it comes from (O23).",
  },
  {
    name: "trait_import_scope",
    pattern: /^use\s[^;]*\sas\s_;/,
    glob: "crates/**/*.rs",
    why: "A trait imported only for its methods is imported where it is used, or left at the top with a row: a top-level `as _` import shows the reader nothing about the fn that needs it (O22).",
  },
  {
    name: "anyhow_pub",
    scan: countAnyhowPub,
    skip: /(^|\/)(main\.rs|bin\/)|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A public signature returns a typed error, not anyhow. A binary, a CLI and a private helper may keep anyhow.",
  },
  {
    name: "dyn_param",
    scan: countDynParams,
    skip: /(^|\/)(main\.rs|bin\/)|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A library fn takes a generic (T: Trait or impl Trait), not a dyn Trait parameter. A counted row is a trait object that stays, with its reason.",
  },
  {
    name: "pub_vec_return",
    scan: countPubVecReturn,
    skip: /(^|\/)(examples|tests|benches)\/|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A public function returns an iterator, not a Vec. A Vec is allowed where the caller needs an owned collection or the function builds a byte buffer, and each such function has a counted row with its reason.",
  },
  {
    name: "pub_vec_param",
    scan: countPubVecParam,
    skip: /(^|\/)(examples|tests|benches)\/|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A public function takes an impl IntoIterator, not a Vec, when it only iterates. A function that keeps the list takes a Vec, and each such function has a counted row with its reason.",
  },
  {
    name: "pub_impl_into",
    scan: countPubImplInto,
    skip: /(^|\/)(examples|tests|benches)\/|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A public function takes impl Into<T> only where call-site ergonomics matter most in a library. Each such function has a counted row with its reason.",
  },
  {
    name: "app_impl_into",
    pattern: /impl Into</,
    skip: /^(?!(?:.*\/)?(?:main\.rs$|bin\/))/,
    glob: "*.rs",
    why: "Application code does not take impl Into<T>. A binary or a bin/ target takes a concrete type.",
  },
  {
    name: "accessor_name",
    scan: countAccessorNames,
    skip: /(^|\/)(examples|tests|benches)\/|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A public accessor follows the guide: no get_ prefix, replace_ returns the old value, take_ clears through &mut self, _mut returns &mut, set_ takes its value by value.",
  },
  {
    name: "bool_param_comment",
    scan: countBoolParamNoComment,
    skip: /(^|\/)(examples|tests|benches)\/|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A public function with a bool parameter writes its purpose in a comment above the parameter, so a call site says what true means.",
  },
  {
    name: "self_convention",
    scan: countSelfConvention,
    skip: /(^|\/)(examples|tests|benches)\/|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A public as_ or to_ method takes a reference, and an into_ method takes self. A by-value as_ or to_ is a Copy type, and it has a counted row.",
  },
  {
    name: "generic_name",
    scan: countGenericNames,
    skip: /(^|\/)(examples|tests|benches)\/|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "A generic parameter is T, U, E, K, V, F, I, R or S, or a CamelCase word of three letters or more.",
  },
  {
    name: "abbrev_name",
    scan: countAbbrevNames,
    skip: /(^|\/)(examples|tests|benches)\/|^(?!crates\/[^/]+\/src\/)/,
    glob: "crates/**/*.rs",
    why: "An identifier is not a letter-deleted abbreviation: write ctx as context, addr as address, and so on. A name that a crate outside our control fixes has a counted row.",
  },
];

// The files git knows, tracked or not yet, minus what .gitignore hides: the same list `git ls-files
// -co --exclude-standard` gives. -z keeps a file name with a space whole.
function listFiles(glob: string): string[] {
  const proc = spawnSync("git", ["ls-files", "-z", "-co", "--exclude-standard", "--", glob], {
    cwd: ROOT,
    encoding: "utf8",
  });
  return (proc.stdout ?? "").split("\0").filter((file) => file.length > 0);
}

// Count the matching lines of each file. A file with no match is left out, as the old check did.
function scan(rule: Rule): Map<string, number> {
  const counts = new Map<string, number>();
  for (const file of listFiles(rule.glob)) {
    if (!existsSync(path.join(ROOT, file))) continue;
    if (rule.skip?.test(file)) continue;
    const text = readFileSync(path.join(ROOT, file), "utf8");
    const count = rule.scanFile
      ? rule.scanFile(file)
      : rule.scan
        ? rule.scan(text)
        : text.split("\n").filter((line) => rule.pattern?.test(line)).length;
    if (count > 0) counts.set(file, count);
  }
  return counts;
}

type Allowed = Map<string, { count: number; reason: string }>;

// The allow list: `rule|file|count|reason`. Blank lines and `#` comments are skipped.
function readAllow(shownAs: string): { allowed: Allowed; problems: string[] } {
  const allowed: Allowed = new Map();
  const problems: string[] = [];
  const file = path.join(ROOT, shownAs);
  if (!existsSync(file)) return { allowed, problems };
  for (const line of readFileSync(file, "utf8").split("\n")) {
    if (/^\s*(#|$)/.test(line)) continue;
    const [rule, target, count, ...reason] = line.split("|");
    const key = `${rule}|${target}`;
    if (allowed.has(key)) {
      problems.push(`style-check: ${shownAs} has two rows for [${rule}] ${target}. Merge them.`);
    }
    allowed.set(key, { count: Number(count) || 0, reason: reason.join("|") });
  }
  return { allowed, problems };
}

function main(): number {
  const { allowed, problems } = readAllow(allowFile);
  const found = new Map<string, number>(); // key: rule|file
  for (const rule of RULES) {
    for (const [file, count] of scan(rule)) found.set(`${rule.name}|${file}`, count);
  }
  const why = new Map(RULES.map((rule) => [rule.name, rule.why]));

  for (const [key, count] of [...found].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
    const [rule = "", file = ""] = key.split("|");
    const limit = allowed.get(key)?.count ?? 0;
    if (count > limit) {
      problems.push(
        `style-check: [${rule}] ${file}: ${count} matching line(s), ${limit} allowed. ${why.get(rule)}`,
      );
    }
  }
  for (const [key, entry] of [...allowed].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
    const have = found.get(key) ?? 0;
    if (have < entry.count) {
      const [rule, file] = key.split("|");
      problems.push(
        `style-check: [${rule}] ${file}: ${have} matching line(s), ${entry.count} allowed. Lower the count in ${allowFile}.`,
      );
    }
  }
  for (const problem of problems) console.log(problem);
  return problems.length > 0 ? 1 : 0;
}

process.exit(main());
