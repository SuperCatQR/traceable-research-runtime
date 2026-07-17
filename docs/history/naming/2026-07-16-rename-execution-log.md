# Rename Execution Log

> Historical execution record retained for review. `CONTEXT.md` defines the current ubiquitous language.

Date: 2026-07-16
Status: historical rename record; current clarification contract superseded

> The log below records the rename state before the model-led natural-dialogue redesign. Current
> code uses `AwaitingUserMessage` and `ResearchReady`; it exposes neither the `/api/sessions` nor
> `/api/intakes` compatibility routes described below. Clarification schema v5 intentionally does
> not retain v2/v3/v4 lifecycle compatibility. See [ADR 0006](../../adr/0006-model-led-dialogue-and-tiered-trace-disclosure.md).

## Baseline

- Core: 94 tests passed; one live end-to-end test intentionally ignored.
- Demo host: 4 tests passed.
- Rust format and Clippy passed.
- Frontend type check and production build passed.

## Batch 1: Domain Vocabulary

Moved modules:

- `src/session.rs` -> `src/conversation.rs`
- `src/intake.rs` -> `src/clarification.rs`
- `src/types.rs` -> `src/research_domain.rs`
- `src/orchestration.rs` -> `src/research_run.rs`

Key changes:

- `ResearchSessionContext` -> `ResearchConversation`
- `ConversationTurn` -> `CompletedResearchTurn`
- `IntakeSession` -> `ClarificationState`
- `IntakeStatus` -> `ClarificationStatus`
- `ResearchSession` -> `ResearchRunExecutor`
- `ResearchBackend` -> `ResearchExecutionBackend`
- `Query` / `Answer` / `Claim` / `Excerpt` -> `SearchQuery` / `GroundedResearchAnswer` / `GroundedClaim` / `SnapshotNavigationExcerpt`
- Clarification lifecycle variants now state their role: `AwaitingUserReply`, `BriefCompleted`, `ResearchPrepared`, and `ModelRequestFailed`.

Compatibility: explicit Serde names preserve existing clarification and research trace event tags.

Gate: core format, Clippy, and 94 tests passed; demo host tests passed.

## Batch 2: Runtime And External Adapters

Moved modules:

- `src/app.rs` -> `src/runtime.rs`
- `src/backend.rs` -> `src/live_research_backend.rs`
- `src/adapters.rs` -> `src/external_adapters.rs`
- `src/error.rs` -> `src/research_error.rs`
- `src/snapshot.rs` -> `src/snapshot_store.rs`
- `src/trace.rs` -> `src/research_trace.rs`

Key changes:

- `AppConfig` -> `ResearchRuntimeConfig`
- `ResearchService` -> `TraceableResearchRuntime`
- `PreparedRun` -> `PreparedResearchRun`
- `run` -> `execute_prepared_research` at the public runtime interface
- `SearxngClient` -> `SearxngSearchClient`
- `CrawlClient` -> `Crawl4AiSnapshotClient`
- `StrongClient` -> `OpenAiCompatibleModelClient`
- Research backend actions now name their objects: `generate_search_queries`, `search_web`, `capture_web_snapshot`, `select_evidence_snapshots`, and `synthesize_grounded_answer`.
- Duplicate `read_session` / `get_session` entry points collapsed into `load_conversation`.

Gate: core compilation, format, Clippy, and 94 tests passed.

## Batch 3: Demo Transport

Key changes:

- `AppState` -> `DemoHostState`
- Capability maps now distinguish conversation capability tokens from clarification capability tokens.
- Request and response types name the command or projection they carry.
- Route handlers use a `*_handler` suffix while runtime commands do not.
- Resolver names distinguish public capability tokens from internal persistence IDs.

Compatibility: `/api/sessions` and `/api/intakes` routes remain available for the pre-extension Demo.

Gate: demo-host format, Clippy, and 4 tests passed.

## Batch 4: Frontend

Moved module:

- `demo/src/api.ts` -> `demo/src/research-client.ts`

Key changes:

- Response types use Conversation, Clarification, Grounded Claim, and Evidence Source vocabulary.
- Active UI state names identify the active conversation and active clarification.
- `perform` and `processIntake` became `executeClarificationRequest` and `handleClarificationResponse`.
- Boolean state reads as `requestIsInProgress`.

Manual contract review caught three changes that TypeScript accepted but the running Demo would not:

- `pending_turns` had been accidentally changed by a broad `pending` replacement.
- Compatibility `/intakes` paths had been accidentally changed by a broad `intake` replacement.
- The `.messages` CSS hook had been accidentally changed with the in-memory message variable.

All three external contracts were restored while the local identifiers retained their precise names.

Gate: TypeScript check and production build passed.

## Deliberately Retained Compatibility Names

- Persisted directory names: `data/sessions`, `data/intake`, and `data/traces`.
- Persisted fields such as `session_id` and `turn`.
- Existing event tags such as `session_started`, `intake_started`, `run_prepared`, and `answer`.
- Existing Demo `/api/sessions` and `/api/intakes` routes.
- Existing environment variables, including `STRONG_MODEL_*`, until the Model Profile migration supplies a versioned replacement.

These names are retained as external data contracts, not recommended as new local identifiers.

## Final Scan

- No retired public code identifiers remain in Rust or TypeScript source.
- Current README and architecture documents reference the renamed modules and runtime commands.
- Historical decision records and the naming audit retain old terms where they document the previous state.
