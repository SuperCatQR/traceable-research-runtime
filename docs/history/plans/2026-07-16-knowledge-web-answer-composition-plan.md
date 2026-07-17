# Knowledge and Web Answer Composition Plan

> Historical implementation plan. Current product behavior is defined by ADR 0006 and the architecture document.

## Acceptance Criteria

1. A new Research Turn accepts `web_first` or `knowledge_first`; omitted input defaults to
   `web_first` (20:80 knowledge:web).
2. Each run creates an auditable, Web-independent model knowledge draft, retrieves and validates
   Web Snapshots as before, then returns a comparison and weighted final answer.
3. The final answer exposes the origin of every claim. Web evidence remains linked to archived
   sources; model knowledge is explicitly marked and never fabricated as a citation.
4. A user can select the style before any new turn in a persisted conversation. The selected
   style is shown again when replaying that turn.
5. Existing trace, catalogue, workspace, and legacy HTTP behavior remain readable with the
   default `web_first` mode.
6. The target server uses a safe DNS/proxy configuration and a multi-engine SearXNG search setup
   before its deployed demo is accepted.

## Implementation Flow

1. Add the core style, knowledge draft, comparison, origin-aware claim contracts, and trace v4.
2. Add the run executor's independent knowledge pass and final reflective composition prompt.
3. Persist the frozen style through the SQLite catalogue and authenticated workspace API.
4. Add the per-turn selector and origin/comparison rendering to the demo.
5. Run unit, API, and browser checks; inspect the target host network before deploying.
