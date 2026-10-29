import { expect, test } from "bun:test";
import { nextPatch, previewTag } from "./preview-version";

test("the next version is one patch above the Cargo version", () => {
  expect(nextPatch("0.12.10")).toBe("0.12.11");
});

test("the preview tag numbers the commits since the last stable tag", () => {
  expect(previewTag("0.12.10", 7)).toBe("v0.12.11-preview.7");
});
