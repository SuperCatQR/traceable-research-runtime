import { describe, expect, it } from "vitest";
import { safeEvidenceUrl } from "./format";

describe("safeEvidenceUrl", () => {
  it.each([
    ["https://example.com/source?q=1", "https://example.com/source?q=1"],
    ["http://example.com/source", "http://example.com/source"],
  ])("accepts evidence links over HTTP(S)", (value, expected) => {
    expect(safeEvidenceUrl(value)).toBe(expected);
  });

  it.each([
    "javascript:alert(1)",
    "data:text/html,unsafe",
    "file:///etc/passwd",
    "/relative/path",
    "not a URL",
  ])("rejects unsafe or non-absolute evidence links", (value) => {
    expect(safeEvidenceUrl(value)).toBeUndefined();
  });
});
