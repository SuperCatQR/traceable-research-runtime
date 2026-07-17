# Knowledge and Web Answer Composition Execution Log

> Historical execution record; it is retained as verification evidence, not a current contract.

## 2026-07-16

1. Added `ResearchAnswerStyle`: `web_first` defaults to 20:80 knowledge:web and
   `knowledge_first` is 80:20. The choice is frozen per Research Turn, run header, and catalogue
   record.
2. Added independent `ModelKnowledgeDraft`, reflective composition, origin-aware final claims,
   and initial trace schema v4 with v1-v3 replay compatibility.
3. Added explicit concise rationale contracts for clarification, knowledge drafts, query planning,
   source selection, final claims, and final weighted synthesis. These are review summaries, not
   hidden chain-of-thought.
4. Added an ownership-protected turn trace API that returns linked clarification and research
   JSONL events.
5. Added the per-turn answer-style selector and answer provenance/comparison rendering to the
   demo workspace.
6. Verified core tests, demo-host tests, TypeScript checking, and production frontend build before
   deployment preparation.
7. Server-network probe found Google and DuckDuckGo HTTPS unavailable; existing SearXNG Bing
   parsing also returned no usable results. The runtime therefore keeps SearXNG as primary and
   uses the reachable Bing RSS endpoint only as a controlled fallback, with the same downstream
   URL validation and snapshot audit.
8. Tightened the audit contract after deployment review: clarification schema v2 and research
   trace schema v5 validate rationale length during both persistence and replay. Historical v1/v4
   data stays readable but is explicitly marked `legacy_unverified`.
9. Added an ownership-protected pre-run Trace response and a lazy frontend decision timeline for
   clarification, knowledge draft, query, source selection, claim, and synthesis rationales.
10. Deployed release `20260716-811bad1` to `192.168.1.71:8090` after checking the `apps-net`
    dependency topology, free port, archive hash, container health, and credential-file modes.
11. Rolled the final audit-contract release `20260716-b79cd83` to the same endpoint. The first
    server-side build held an SSH output pipe and the next build waited on the registry index, so
    the final build reused the verified prior Rust builder layer while copying the new release
    source. This kept the old container live until a distinct image `e9d1f29a12ae` was ready.
12. Final server verification confirmed the v5 audit marker in the running binary, the `apps-net`
    attachment, `200 ok` health response, owner-gated Trace route, release pointer, and `0600`
    runtime credential files.
