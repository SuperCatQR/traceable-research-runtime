import assert from "node:assert/strict";
import test from "node:test";

import { ResearchTraceAuditCache } from "../src/research-trace-audit-cache.ts";

test("audit pages are cached by both turn and stage", () => {
  const cache = new ResearchTraceAuditCache();
  const turnASearchPage = { label: "turn-a-search" };
  const turnBArchivePage = { label: "turn-b-archive" };

  cache.set("turn-a", "search", turnASearchPage);
  cache.set("turn-b", "archive", turnBArchivePage);

  assert.equal(cache.get("turn-a", "search"), turnASearchPage);
  assert.equal(cache.get("turn-b", "archive"), turnBArchivePage);
  assert.equal(cache.get("turn-a", "archive"), undefined);
  assert.equal(cache.get("turn-b", "search"), undefined);
});
