# Compose research answers from model knowledge and web evidence

- Status: accepted
- Date: 2026-07-16

> The composition and provenance decision remains current. ADR 0006 supersedes the user-selectable
> chat control and the Trace v4 compatibility statements below: the demo fixes `web_first` for its
> natural-language chat path and uses Trace schema v6 in a new data volume.

## Context

The original runtime accepts only claims backed by archived Web Snapshots. That is correct for a
strictly grounded research product, but it suppresses useful model reasoning for exploratory work
and makes it impossible for a user to see where model knowledge and newly retrieved evidence
agree or disagree.

Two answer styles are available to the runtime for every Research Turn:

- `web_first` (default): model knowledge 20%, Web evidence 80%; use for fact-sensitive work.
- `knowledge_first`: model knowledge 80%, Web evidence 20%; use for exploratory discussion.

The runtime selection is frozen once a Research Run is prepared so retries and replay cannot
silently change its meaning. The demo chat fixes `web_first` rather than exposing a separate
user control.

## Decision

1. Add `ResearchAnswerStyle` to the core domain. Store it on the prepared run, run header, demo
   catalogue turn record, API response, and final answer response. Its default is `web_first`.
2. Generate an independent structured `ModelKnowledgeDraft` from only the frozen Research Brief
   and prior completed-turn context. It never receives Web Snapshots and is recorded in the
   append-only trace.
3. Keep the existing Web Search, snapshot capture, evidence selection, and SSRF protections
   unchanged. The final model call receives both the independent draft and selected snapshots,
   then returns a weighted answer plus an explicit comparison.
4. Every final claim declares its origin. `web_evidence` claims require one or more selected
   snapshot references; `model_knowledge` claims must have none. The browser presents the latter
   as model knowledge rather than disguising it as a citation.
5. At the time of this decision the run trace moved to schema v4. The current schema and data
   volume boundary are governed by ADR 0006.

## Consequences

- Users can deliberately trade factual grounding for breadth without losing provenance.
- A model's knowledge-cutoff limitations remain visible; model-knowledge claims are not promoted
  to Web evidence.
- One additional model call is made per run. It is intentionally independent from Web retrieval,
  so failure of either source is visible in the trace rather than silently blended.
- Persisting answer style requires a catalogue migration; active turns retain their frozen style.
