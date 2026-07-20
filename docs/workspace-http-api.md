# Workspace HTTP API

All routes are under `/api`. Except for registration, login, and health checks,
requests require the same-origin `traceable_login` cookie. Every `/api`
response uses `Cache-Control: no-store`.

Errors are JSON:

```json
{
  "code": "not_found",
  "message": "Public, actionable message",
  "retryable": false
}
```

Malformed or schema-invalid JSON returns `400 invalid_json`. A resource owned
by another account is reported as `404 not_found`.

## Transport And Session Rules

- JSON request bodies require `Content-Type: application/json`, reject unknown
  fields, and are limited to 16 KiB.
- Registration and login set `traceable_login` with `Path=/`, `HttpOnly`,
  `SameSite=Strict`, a 30-day maximum age, and deployment-controlled `Secure`.
- Logout requires a valid Login Session, revokes its stored token hash, expires
  the cookie, and returns `204`.
- Requests with an untrusted Host or Origin return
  `403 request_not_allowed`. Production is same-origin and does not expose CORS.
- Unless `DEMO_ALLOW_PRIVATE_MODEL_ENDPOINTS=true`, Model Profile endpoints must
  resolve only to public addresses both when saved and immediately before each
  model request. The request is connected directly to those checked addresses
  while retaining the original hostname for Host/TLS validation. Model requests
  never follow redirects, including when private endpoints are explicitly
  enabled.
- Public IDs are opaque strings. Catalog timestamps are Unix seconds. Audit
  `occurred_at` values are RFC 3339 strings from the append-only Trace.
- Active Model Profiles sort default-first then by recent update. Active
  Conversations sort by `updated_at DESC`; Turns sort by `turn_number ASC`;
  archive lists sort by `archived_at DESC`.

## Endpoints

| Method | Path | Success |
| --- | --- | --- |
| `GET` | `/auth/me` | `200 UserAccount` |
| `POST` | `/auth/register` | `200 UserAccount` |
| `POST` | `/auth/login` | `200 UserAccount` |
| `POST` | `/auth/logout` | `204` |
| `GET` | `/model-profiles` | `200 ModelProfile[]` |
| `POST` | `/model-profiles` | `200 ModelProfile` |
| `PATCH` | `/model-profiles/{profile_id}` | `200 ModelProfile` |
| `POST` | `/model-profiles/{profile_id}/default` | `204` |
| `POST` | `/model-profiles/{profile_id}/verify` | `204` |
| `DELETE` | `/model-profiles/{profile_id}` | `204` |
| `GET` | `/archives/model-profiles` | `200 ArchivedModelProfile[]` |
| `POST` | `/model-profiles/{profile_id}/restore` | `200 ModelProfile` |
| `GET` | `/conversations` | `200 ConversationSummary[]` |
| `POST` | `/conversations` | `200 ConversationDetail` |
| `GET` | `/conversations/{conversation_id}` | `200 ConversationDetail` |
| `PATCH` | `/conversations/{conversation_id}` | `200 ConversationSummary` |
| `DELETE` | `/conversations/{conversation_id}` | `204` |
| `GET` | `/archives/conversations` | `200 ArchivedConversationSummary[]` |
| `POST` | `/conversations/{conversation_id}/restore` | `200 ConversationSummary` |
| `POST` | `/conversations/{conversation_id}/turns` | `200 ChatResearchTurn` |
| `POST` | `/conversations/{conversation_id}/turns/{turn_id}/messages` | `200 ChatResearchTurn` |
| `GET` | `/conversations/{conversation_id}/turns/{turn_id}/trace/summary` | `200 ResearchTraceSummary` |
| `GET` | `/conversations/{conversation_id}/turns/{turn_id}/trace/audit` | `200 ResearchTraceAuditPage` |

`GET /health` is the deployment health check and is not one of the 23 product
endpoints.

## Request Bodies

| Operation | Accepted JSON fields |
| --- | --- |
| Register | `email`, `password`, `display_name` |
| Login | `email`, `password` |
| Create Model Profile | `display_name`, `api_base_url`, `api_key`, `model_id`, `make_default?` |
| Update Model Profile | Any non-empty subset of `display_name`, `api_base_url`, `api_key`, `model_id` |
| Create Conversation | `model_profile_id?` |
| Update Conversation | Any non-empty subset of `title`, `model_profile_id` |
| Restore Conversation | Empty body or `model_profile_id?` |
| Create Turn | `question`, `answer_style` (`web_first` or `knowledge_first`) |
| Submit Dialogue Message | `revision`, `message` |

## Response Allowlists

| DTO | Public fields |
| --- | --- |
| `UserAccount` | `user_id`, `email`, `display_name`, `created_at` |
| `ModelProfile` | `profile_id`, `display_name`, `api_base_url`, `model_id`, `revision`, `is_default`, `has_api_key`, `verified_at`, `created_at`, `updated_at` |
| `ConversationSummary` | `conversation_id`, `title`, `model_profile_id`, `model_profile_name`, `turn_count`, `latest_turn_status`, `created_at`, `updated_at` |
| `ConversationDetail` | All summary fields plus ordered `turns` |
| `ChatResearchTurn` | `turn_id`, `turn_number`, `user_question`, `status`, `answer`, `dialogue`, `created_at`, `updated_at`, `completed_at` |
| `ResearchTraceSummary` | `model_id`, `understanding`, `rounds`, source counts, `selected_sources`, `synthesis_rationale`, `failure` |
| `ResearchTraceAuditPage` | `next_cursor`, `entries` |

Archived DTOs add only archive metadata: Model Profile adds `archived_at`;
Conversation adds `archived_at` and `model_profile_available`.

Frozen examples live in `docs/fixtures/workspace-l1-turn.json`,
`workspace-l2-trace-summary.json`, `workspace-l3-trace-audit.json`, and
`workspace-error.json`.

## Idempotency

The following operations require an `Idempotency-Key` header:

- Create a model profile.
- Create a conversation.
- Create a research turn.
- Submit a dialogue message.

The scope is account, method, resource path, and key. After the completion
record has been persisted, an identical retry replays the original status and
JSON without running the handler again. Reusing a key with a different request
returns `409 idempotency_key_reused`. A concurrent retry returns
`409 idempotency_request_in_progress` with `retryable: true`.
Completed records are retained for at least 24 hours and cleaned lazily.
An in-progress claim may be taken over after five minutes. Every claim and
takeover receives a new fencing token; only the current token may complete or
complete that record, so a stale request cannot overwrite its successor.
Normal request-drop cleanup never deletes an in-progress record. A
serialization key can additionally hold one active operation for a shared
Conversation or Turn even when the requests use different idempotency keys.
The durable operation ID is retained across takeover. Protected writes derive
their resource IDs from that operation and reuse the same Runtime conversation,
clarification, and dialogue evidence when a request is resumed.

The Catalog resource mutation and completed response snapshot are committed in
one SQLite transaction. Runtime JSONL logs repair torn tails and reopen a
reserved seed without appending a duplicate start or user message.

## Archive Recovery

Archive operations preserve resource IDs and turn history. Restored model
profiles retain their encrypted credential and verification timestamp. A
conversation whose original model profile is archived must be restored with an
active replacement `model_profile_id`; otherwise the API returns
`409 conversation_model_profile_archived`.

## Browser Data Boundaries

`ChatResearchTurn` is the L1 browser projection. It contains the user question,
dialogue, answer Markdown, and necessary source title/URL pairs. It does not
contain run IDs, model endpoints, API keys, briefs, knowledge drafts, claim
rationales, snapshot references, or answer-composition internals.

`ResearchTraceSummary` is the on-demand L2 projection and includes the model ID
locked for that turn. `ResearchTraceAuditPage` is the on-demand L3 projection.
Neither response exposes a run ID or internal audit flags. Audit `limit` must be
between 1 and 100; stage and cursor values are validated by the server.

## Stable Conflicts

- `profile_name_already_exists`
- `model_profile_in_use_by_active_turn`
- `model_profile_in_use_by_conversation`
- `conversation_has_active_turn`
- `conversation_model_profile_archived`
- `conversation_model_profile_changed`
- `model_profile_changed`
- `dialogue_revision_conflict`
- `turn_not_accepting_messages`
- `idempotency_key_reused`
- `idempotency_request_in_progress`
- `idempotency_operation_blocked`

## Other Stable Errors

- `invalid_json`, `invalid_request`, `invalid_email`, `invalid_password`
- `invalid_display_name`, `invalid_profile_name`, `invalid_model_id`
- `invalid_model_endpoint`, `private_model_endpoint_blocked`, `invalid_api_key`
- `invalid_conversation_title`, `invalid_question`, `invalid_dialogue_message`
- `invalid_trace_stage`, `invalid_trace_cursor`, `invalid_trace_limit`
- `idempotency_key_required`, `invalid_idempotency_key`
- `authentication_required`, `invalid_credentials`, `request_not_allowed`
- `email_already_registered`, `model_profile_required`, `not_found`
- `model_verification_failed`, `internal_error`

## Catalog Migrations

The current Catalog schema is v7. `0003` introduces archive/idempotency
records, `0004` makes Model Profile names unique only while active, `0005`
adds idempotency claim fencing, `0006` enforces one nonterminal Turn per
Conversation, and `0007` persists an immutable operation identity, claim
timestamp, and optional serialization key. Takeover changes only the fencing
token and claim timestamp. Idempotency cleanup removes completed records only;
legacy v6 `in_progress` records are migrated to fail-closed `blocked` records
and are never silently released.

## Open Delivery Gate

The local reconciliation path is implemented: operation/resource IDs are
deterministic after reservation, Runtime side effects are replayable, and the
Catalog resource plus response snapshot share one fenced SQLite transaction.
Legacy v6 in-progress rows remain fail-closed instead of being re-executed.

The full delivery gate still requires fault-injection and Compose restart
acceptance. In particular, an OpenAI-compatible provider that does not honor an
idempotency header can receive a second billable request if the process exits
after the provider responds but before the local model outcome is durable. That
external limitation must not be described as exactly-once provider execution.
