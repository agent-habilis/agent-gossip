import { expect, test } from "bun:test";
import { isValidBody, isValidSquareName } from "./index";

// These mirror the Rust protocol invariants (MessageBody::new and
// MeshName::new). If they drift, the client accepts input the daemon rejects
// (or vice versa) — a silent mismatch types and lint can't see.

test("isValidBody allows text, unicode, and tab/newline/CR", () => {
  expect(isValidBody("hello world")).toBe(true);
  expect(isValidBody("café 💬 こんにちは")).toBe(true);
  expect(isValidBody("line1\nline2\twith tab\r")).toBe(true);
  expect(isValidBody("")).toBe(true);
});

test("isValidBody rejects control chars other than tab/newline/CR", () => {
  expect(isValidBody("bell\x07")).toBe(false);
  expect(isValidBody("nul\x00")).toBe(false);
  expect(isValidBody("esc\x1b")).toBe(false);
  expect(isValidBody("del\x7f")).toBe(false);
  expect(isValidBody("c1\x85")).toBe(false);
});

test("isValidSquareName accepts 1-32 scalar values without forbidden symbols", () => {
  expect(isValidSquareName("cool-team")).toBe(true);
  expect(isValidSquareName("a")).toBe(true);
  expect(isValidSquareName("a".repeat(32))).toBe(true);
  expect(isValidSquareName("café")).toBe(true);
  // Counted by code point, not UTF-16 units: 32 emoji is still length 32.
  expect(isValidSquareName("💬".repeat(32))).toBe(true);
  // Path separators are allowed — a square name may be a URL.
  expect(isValidSquareName("github.com/acme/repo")).toBe(true);
  expect(isValidSquareName("a\\b")).toBe(true);
});

test("isValidSquareName rejects bad length, whitespace, control, and < > #", () => {
  expect(isValidSquareName("")).toBe(false);
  expect(isValidSquareName("a".repeat(33))).toBe(false);
  expect(isValidSquareName("has space")).toBe(false);
  expect(isValidSquareName("tab\tname")).toBe(false);
  expect(isValidSquareName("ctrl\x01")).toBe(false);
  for (const bad of ["a<b", "a>b", "a#b"]) {
    expect(isValidSquareName(bad)).toBe(false);
  }
});
