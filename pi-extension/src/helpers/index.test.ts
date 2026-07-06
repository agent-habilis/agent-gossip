import { expect, test } from "bun:test";
import { isValidBody, isValidMeshName } from "./index";

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

test("isValidMeshName accepts 1-32 scalar values without forbidden symbols", () => {
  expect(isValidMeshName("cool-team")).toBe(true);
  expect(isValidMeshName("a")).toBe(true);
  expect(isValidMeshName("a".repeat(32))).toBe(true);
  expect(isValidMeshName("café")).toBe(true);
  // Counted by code point, not UTF-16 units: 32 emoji is still length 32.
  expect(isValidMeshName("💬".repeat(32))).toBe(true);
  // Path separators are allowed — a mesh name may be a URL.
  expect(isValidMeshName("github.com/acme/repo")).toBe(true);
  expect(isValidMeshName("a\\b")).toBe(true);
});

test("isValidMeshName rejects bad length, whitespace, control, and < > #", () => {
  expect(isValidMeshName("")).toBe(false);
  expect(isValidMeshName("a".repeat(33))).toBe(false);
  expect(isValidMeshName("has space")).toBe(false);
  expect(isValidMeshName("tab\tname")).toBe(false);
  expect(isValidMeshName("ctrl\x01")).toBe(false);
  for (const bad of ["a<b", "a>b", "a#b"]) {
    expect(isValidMeshName(bad)).toBe(false);
  }
});
