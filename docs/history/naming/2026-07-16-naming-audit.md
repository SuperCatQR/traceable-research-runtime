# Naming Audit

> Historical naming audit retained for review. `CONTEXT.md` defines the current ubiquitous language.

Date: 2026-07-16
Scope: `src/`, `demo-host/src/`, `demo/src/`, persisted JSON/SQLite names, HTTP routes, environment variables, tests, and documentation.

## Standard Applied

A name must identify the domain concept or action at its call site without a nearby explanatory comment. A rename is justified only when the current name is ambiguous, factually inaccurate, overloaded across concepts, or hides an important external contract. Familiar short names remain when their scope makes them unambiguous.

The audit does not enforce one CRUD verb or expand conventional terms such as `url`, `id`, `api`, `http`, `json`, `html`, `sql`, `ssrf`, `dto`, or `env`.

## Baseline

- Core library: 94 tests passed; one live end-to-end test remains intentionally ignored.
- Demo host: 4 tests passed.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --all-targets -- -D warnings` passed.
- Frontend TypeScript check and production build passed.
- The worktree was already dirty. Existing changes are treated as the current source of truth and will not be reverted.

## Domain Collisions

| Current name | Problem | Chosen name |
|---|---|---|
| `session` / `ResearchSessionContext` | Means the durable conversation, while other modules use session for clarification and execution | `ResearchConversation` |
| `ConversationTurn.turn` | A bare number does not say that it is an ordered position | `turn_number` |
| `PendingTurn` | The same shape is also used for cancelled and failed turns | `ResearchTurnSummary` with an explicit outcome projection, or outcome-specific types |
| `IntakeSession` | Not a user session; it is the projected state of clarification | `ClarificationState` |
| `IntakeStatus` | Status of clarification, not generic intake | `ClarificationStatus` |
| `IntakeEvent*` | Events belong to the clarification state machine | `ClarificationEvent*` |
| `ResearchSession` in orchestration | Represents one executable run, not a conversation | `ResearchRunExecutor` |
| `history` | Hides whether it is browser messages, completed turns, or model context | `conversation_history` or `completed_turn_history` according to scope |

## Core Library

| Current name | Problem | Chosen name |
|---|---|---|
| `app.rs` | Generic module name for the public runtime facade | `runtime.rs` |
| `AppConfig` | Does not identify which application or what is configured | `ResearchRuntimeConfig` |
| `ResearchService` | Generic service suffix hides that it owns the runtime workflow | `TraceableResearchRuntime` |
| `new_run_id` | `new` hides generation and uniqueness intent | `generate_research_run_id` |
| `trace_path` | Does not identify which trace | `research_trace_path` |
| `prepare_run` | Run type and precondition are hidden | `prepare_research_run` |
| `run` | Critical public command with no object or outcome | `execute_prepared_research` |
| `advance_intake` | Overloaded intake term | `advance_clarification` |
| `PreparedRun` | Missing domain qualifier | `PreparedResearchRun` |
| `PublicAnswer` | Public to whom and for what is unclear | `ResearchAnswerResponse` |
| `PublicClaim` / `PublicSource` | Transport projection is implied but unnamed | `GroundedClaimResponse` / `EvidenceSourceResponse` |
| `backend.rs` | Generic module name | `live_research_backend.rs` |
| `LiveBackend` | Live implementation of which behavior is unclear | `LiveResearchBackend` |
| `StrongClient` | "Strong" is a subjective tier, not a protocol or role | `OpenAiCompatibleModelClient` |
| `SearxngClient` | Omits the action supplied by the adapter | `SearxngSearchClient` |
| `CrawlClient` | Omits the concrete adapter and returns a snapshot, not a generic crawl | `Crawl4AiSnapshotClient` |
| `types.rs` | Every module contains types; it gives no domain signal | `research_domain.rs` |
| `Query` | Ambiguous beside user questions and SQL | `SearchQuery` |
| `Answer` | Ambiguous beside clarification answers and HTTP responses | `GroundedResearchAnswer` |
| `Excerpt` | Omits the source and navigation purpose | `SnapshotNavigationExcerpt` |
| `orchestration.rs` | Describes a technique instead of the owned domain concept | `research_run.rs` |
| `ResearchBackend::plan` | Does not say what is planned | `generate_search_queries` |
| `ResearchBackend::search` | Does not identify the searched world | `search_web` |
| `ResearchBackend::crawl` | Implementation verb; contract is immutable capture | `capture_web_snapshot` |
| `ResearchBackend::select` | Does not say what is selected | `select_evidence_snapshots` |
| `ResearchBackend::synthesize` | Does not say the required result | `synthesize_grounded_answer` |
| `ResearchState` | State of what and at which scope is hidden | `ResearchRunProgress` |
| `StopReason` | Omits the process that stopped | `ResearchRunStopReason` |
| `ResearchResult` | Conflicts with the generic meaning of `Result` | `ResearchRunOutput` |
| `snapshot.rs` | The module owns SQLite persistence, not the snapshot entity | `snapshot_store.rs` |
| `SnapshotWriter` / `SnapshotReader` | Two shallow names split one SQLite persistence contract | `SnapshotStore` with read-only access made explicit where needed |
| `trace.rs` | Trace purpose is hidden | `research_trace.rs` |
| exported `Result<T>` | Too broad at the crate root | `ResearchResult<T>` only inside the error module; do not root-export |

## Demo Host

| Current name | Problem | Chosen name |
|---|---|---|
| `AppState` | Axum convention is familiar but domain responsibilities are invisible | `DemoHostState` |
| `service` | Generic field | `research_runtime` |
| `sessions` | Maps capability tokens to internal conversation IDs | `conversation_capabilities` |
| `clarifications` | Maps public tokens to bound internal clarification IDs | `clarification_capabilities` |
| `SessionDto` / `IntakeDto` | DTO suffix says format but not semantics | `ConversationResponse` / `ClarificationResponse` |
| `StartRequest` | Start what? | `StartResearchTurnRequest` |
| `ApiError` / `ErrorDto` | Omits the public HTTP contract | `PublicHttpError` / `ErrorResponse` |
| `new_handle` | Neither the object nor security property is named | `generate_capability_token` |
| `resolve_session` | Actually resolves a public token to an internal ID | `resolve_conversation_id` |
| `register_intake` | Actually publishes a clarification capability | `register_clarification_capability` |
| `resolve_intake` | Actually verifies conversation binding | `resolve_bound_clarification_id` |
| `research` / `run_research` | Route handler and workflow helper are indistinguishable | `execute_research_handler` / `execute_research_turn` |
| route `{id}` / `{session}` / `{intake}` | Path variables hide public capability semantics | `{conversation_id}` / `{turn_id}` after the durable ownership extension |

## Frontend

| Current name | Problem | Chosen name |
|---|---|---|
| `api.ts` | Generic transport module name | `research-client.ts` |
| `Source`, `Claim`, `Message`, `Role` | Global names collide easily and omit their domain | `EvidenceSource`, `GroundedClaim`, `ConversationMessage`, `ConversationRole` |
| `messages` | Which messages is unclear once a session list exists | `activeConversationMessages` |
| `sequence` | Does not identify what is sequenced | `nextConversationMessageId` |
| `pending` | A boolean should read as a question | `requestIsInProgress` |
| `intake` | Overloaded term | `activeClarification` |
| `retryAction` | Stored callback applies only to the active failed operation | `retryActiveOperation` |
| `perform` | Critical behavior is hidden | `executeClarificationRequest` |
| `processIntake` | Does not state transition outcome | `handleClarificationResponse` |
| `render` | Acceptable only in a tiny module; becomes ambiguous after UI expansion | `renderActiveConversation` |
| `value` in submit handlers | Hides whether it is a question or clarification reply | `submittedUserText` |

## Names Deliberately Retained

- `url`, `query`, `rank`, `title`, `answer`, `event`, `policy`, `response`, `path`, and `line` remain where the containing type or function gives them one obvious meaning.
- Conventional adapter names `HTTP`, `JSON`, `HTML`, `SSRF`, `SearXNG`, `crawl4ai`, `OpenAI`, and `API` remain unchanged.
- CRUD synonyms will not be normalized mechanically. `create`, `read`, `open`, `append`, and `remove` keep their natural storage semantics.
- Persisted JSON keys, event tags, SQLite columns, and current environment variable names remain stable during the code rename. They are data contracts, not local identifiers.

## Automated Checks Are Not The Naming Policy

Compiler, Clippy, TypeScript, route tests, replay tests, and persistence fixtures catch broken references and compatibility regressions. No dictionary-based naming linter will be added: semantic accuracy requires domain review and cannot be reduced to banned words.
