import { expect, test } from "bun:test";
import { nextVersion, previewTag, tagAction } from "./preview-version";

test("a patch release adds one to the patch number", () => {
  expect(nextVersion("0.12.10", "patch")).toBe("0.12.11");
});

test("a major change on 0.x gives the next minor, as knope does", () => {
  expect(nextVersion("0.12.10", "major")).toBe("0.13.0");
});

test("a minor change on 0.x gives the next patch, as knope does", () => {
  expect(nextVersion("0.12.10", "minor")).toBe("0.12.11");
});

test("a major change on 1.x gives the next major", () => {
  expect(nextVersion("1.2.3", "major")).toBe("2.0.0");
});

test("the preview tag numbers the commits since the version was set", () => {
  expect(previewTag("0.12.10", 7)).toBe("v0.12.11-preview.7");
});

test("a new tag is created when none exists", () => {
  expect(tagAction("v0.12.11-preview.7", undefined, "abc")).toBe("create");
});

test("an existing tag is reused when its commit is built on this HEAD", () => {
  expect(tagAction("v0.12.11-preview.7", "abc", "abc")).toBe("reuse");
});

test("an existing tag built on another HEAD fails loudly", () => {
  expect(() => tagAction("v0.12.11-preview.7", "old", "abc")).toThrow(/exists on another commit/);
});
