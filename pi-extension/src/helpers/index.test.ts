import { expect, test } from "bun:test";
import { isValidBody, isValidSwarmName } from "./index";

// These mirror the Rust protocol invariants (MessageBody::new and
// SwarmName::new). If they drift, the client accepts input the daemon rejects
// (or vice versa) — a silent mismatch types and lint can't see.

test("isValidBody allows text, unicode, and tab/newline/CR", () => {
  expect(isValidBody("hello world")).toBe(true);
  expect(isValidBody("café 🐝 こんにちは")).toBe(true);
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

test("isValidSwarmName accepts 1-32 scalar values without forbidden symbols", () => {
  expect(isValidSwarmName("cool-team")).toBe(true);
  expect(isValidSwarmName("a")).toBe(true);
  expect(isValidSwarmName("a".repeat(32))).toBe(true);
  expect(isValidSwarmName("café")).toBe(true);
  // Counted by code point, not UTF-16 units: 32 emoji is still length 32.
  expect(isValidSwarmName("🐝".repeat(32))).toBe(true);
});

test("isValidSwarmName rejects bad length, whitespace, control, and / \\ < > #", () => {
  expect(isValidSwarmName("")).toBe(false);
  expect(isValidSwarmName("a".repeat(33))).toBe(false);
  expect(isValidSwarmName("has space")).toBe(false);
  expect(isValidSwarmName("tab\tname")).toBe(false);
  expect(isValidSwarmName("ctrl\x01")).toBe(false);
  for (const bad of ["a/b", "a\\b", "a<b", "a>b", "a#b"]) {
    expect(isValidSwarmName(bad)).toBe(false);
  }
});
