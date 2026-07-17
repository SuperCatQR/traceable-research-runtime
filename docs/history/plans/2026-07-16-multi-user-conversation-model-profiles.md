# Multi-user Conversation And Model Profile Extension

> Historical implementation plan retained for review. Current behavior is specified by ADR 0002, ADR 0003, and ADR 0006.

Date: 2026-07-16
Status: implemented; current workspace contract updated for model-led dialogue and tiered Trace

## Outcome

The Demo becomes a local-first, multi-account research workspace. A user can register, sign in, create and reopen multiple Research Conversations, restore prior turns after a browser or host restart, and choose a saved OpenAI-compatible Model Profile without exposing its API key to the browser after submission.

## Scope

### Included

- Email/password User Accounts.
- Revocable, expiring Login Sessions in an HttpOnly cookie.
- Durable conversation list, rename, archive, reopen, and message replay.
- One selected Model Profile per conversation, snapshotted by revision for each Research Turn.
- Create, update, verify, set default, and archive Model Profiles.
- API key encryption at rest with a server-owned key.
- Model-led natural dialogue, automatic research start, research trace, snapshots, and citations.
- Right-side L2 research overview and L3 audit details for each owned Turn.

### Excluded From This Demo

- Email verification, password reset email, OAuth, organizations, and role-based access.
- Sharing a conversation between accounts.
- Billing, quotas, or provider cost calculation.
- Hard deletion of audit logs. Conversations and Model Profiles are archived.
- Migrating unowned pre-extension capability sessions into a User Account.

## Ownership And Storage

The demo host owns identity and catalogue metadata in `data/demo-catalog.sqlite`. The core runtime continues to own append-only clarification, conversation, and research trace logs plus immutable Web Snapshots.

```text
User Account
  |-- Login Sessions
  |-- Model Profiles
  `-- Research Conversations
        `-- Research Turns ----> core clarification/conversation/trace logs
```

The catalogue is authoritative for ownership, public IDs, titles, Model Profile selection, and the full answer projection used to restore the browser. Core logs remain authoritative for clarification transitions, ordered turn invariants, research execution, and audit replay.

## Access Patterns

1. Find a User Account by normalized email during login.
2. Resolve a Login Session by SHA-256 token hash and expiry.
3. List active Research Conversations for one user by `updated_at DESC`.
4. Load one owned conversation and its Research Turns by `turn_number`.
5. Resolve one owned Model Profile and decrypt its credential only while executing a command.
6. Reject Model Profile mutation while an unfinished Research Turn references its current revision.
7. Restore the active clarification after host restart through the catalogue's internal clarification ID.

## Data Model

### User Account

- UUID public identifier.
- Case-normalized unique email.
- Argon2id password hash.
- Display name and UTC timestamps.

### Login Session

- Browser receives a 256-bit random token in `traceable_login`.
- SQLite stores only `SHA-256(token)`.
- Sessions expire after 30 days, can be revoked, and update `last_seen_at` at a bounded cadence.

### Model Profile

- UUID public identifier scoped to one User Account.
- Display name, OpenAI-compatible API base URL, model ID, revision, default flag, and archive timestamp.
- API key encrypted with AES-256-GCM. Nonce and ciphertext are stored separately; associated data binds the ciphertext to `user_id + profile_id`.
- The API key is accepted on create or rotation but never returned. Responses expose only `has_api_key`.
- Only one active default profile is allowed per user through a partial unique index.

### Research Conversation

- UUID public identifier scoped to one User Account.
- Internal core conversation ID is never returned.
- Title, selected Model Profile, created/updated timestamps, and optional archive timestamp.
- Title defaults from the first user question and can be renamed.

### Research Turn

- UUID public identifier and unique `(conversation_id, turn_number)`.
- Internal clarification ID and run ID remain server-only.
- Stores user question, lifecycle status, selected Model Profile ID and revision, endpoint/model metadata snapshot, full answer response JSON, and timestamps.
- Status values: `clarifying`, `ready`, `running`, `completed`, `failed`, `cancelled`. `ready` is an
  internal transient while the Host begins automatic research; it is not a user action state.

## Model Profile Consistency

A Research Turn snapshots Model Profile ID, revision, API base URL, and model ID. The encrypted key stays in the Model Profile row. Updating or archiving a profile is rejected while an unfinished turn references its current revision. Completed turns replay their stored answer and no longer require the credential.

The core runtime configuration is split:

- `ResearchInfrastructureConfig`: SearXNG, crawl4ai, and research data directory.
- `ModelAccessConfig`: API base URL, API key, and model ID supplied per clarification/research command.

This keeps shared infrastructure process-wide while model choice remains user-owned and turn-specific.

## HTTP Interface

### Authentication

- `POST /api/auth/register`
- `POST /api/auth/login`
- `POST /api/auth/logout`
- `GET /api/auth/me`

### Model Profiles

- `GET /api/model-profiles`
- `POST /api/model-profiles`
- `PATCH /api/model-profiles/{profile_id}`
- `POST /api/model-profiles/{profile_id}/default`
- `POST /api/model-profiles/{profile_id}/verify`
- `DELETE /api/model-profiles/{profile_id}` archives the profile.

### Research Conversations

- `GET /api/conversations`
- `POST /api/conversations`
- `GET /api/conversations/{conversation_id}`
- `PATCH /api/conversations/{conversation_id}`
- `DELETE /api/conversations/{conversation_id}` archives the conversation.

### Research Turns

- `POST /api/conversations/{conversation_id}/turns`
- `POST /api/conversations/{conversation_id}/turns/{turn_id}/messages`
- `GET /api/conversations/{conversation_id}/turns/{turn_id}/trace/summary`
- `GET /api/conversations/{conversation_id}/turns/{turn_id}/trace/audit?stage=&cursor=&limit=`

All ownership checks occur in SQL queries that include the authenticated `user_id`. A caller receives `404` for another user's object, preventing identifier enumeration.

The turn-create and message commands return the model's natural dialogue reply. The browser has no
confirmation, dedicated clarification reply, retry, or execute command. When the model returns
`start_research`, the Host freezes the internal Brief and executes the run automatically.

## Security Controls

- Passwords: Argon2id via a maintained password hashing library; plaintext is never logged or stored.
- Login tokens: 256-bit random, HttpOnly, SameSite=Strict, Path=/; Secure is enabled by deployment configuration.
- Session storage: only token hashes are stored.
- Credentials: AES-256-GCM with `DEMO_CREDENTIAL_ENCRYPTION_KEY`; startup fails when the key is absent or malformed.
- Model endpoints: HTTPS public endpoints by default. Private or loopback endpoints require explicit `DEMO_ALLOW_PRIVATE_MODEL_ENDPOINTS=true` for trusted local deployments.
- CSRF: SameSite cookie plus the existing same-origin request check for mutating browser requests.
- Error responses: bounded codes and messages; no upstream body, key, prompt, internal ID, SQL, or path is returned.
- Request limits: account/model fields have explicit character limits; existing body and user text limits remain.
- Research concurrency: `DEMO_MAX_CONCURRENT_RESEARCH`, default `2`.

## Interface Design

Audience: a researcher repeatedly opening, comparing, and continuing source-grounded conversations. The first screen is the working product, not a landing page.

### Tokens

- `paper` `#F7F8F6`: main workspace.
- `ink` `#17211B`: primary text and controls.
- `moss` `#2F6B4F`: live state, selected actions, and the trace rail.
- `cool` `#EAF1F6`: selected conversation and focused evidence.
- `brick` `#B44732`: destructive and failed states only.
- `line` `#D7DDD9`: dividers and input edges.

Typography uses a restrained serif for the product mark and conversation title, a compact sans-serif for operational text, and a monospace utility face for model IDs and trace metadata. No hero-scale typography appears inside the workspace.

### Layout

```text
+----------------------+------------------------------------------------+
| account / new        | conversation title      model selector  user  |
| search conversations +--------------------------------+--------------+
| conversation list    | message / answer / sources               | L2 / L3 |
|                      |                                          | inspector |
| archived link        +--------------------------------+--------------+
|                      | composer                                               |
+----------------------+--------------------------------------------------------+
```

On mobile, the conversation list is a drawer opened by a familiar menu icon. Model Profiles use a focused settings dialog with list and edit views, not nested cards.

The trace surface is a right-side inspector rather than a margin inside each chat message. It opens
to L2 Research Overview and switches to L3 Audit Details only on demand. This preserves a quiet
conversation while retaining a visible route to review.

The existing quiet research-chat direction is retained. The deliberate change is cool-blue conversation selection against a moss trace rail, avoiding a one-note green palette.

## User Flows

### First Use

1. Register or sign in.
2. If no Model Profile exists, open Model Profiles immediately.
3. Save and verify a Model Profile.
4. Create a Research Conversation using the default profile.
5. Submit a question. The model replies naturally with its understanding, accepts ordinary follow-up
   messages when needed, and automatically starts research once it judges the intent sufficient.

### Resume

1. Restore Login Session from cookie.
2. List the user's active conversations.
3. Select the most recently updated conversation.
4. Load ordered turns, full prior answers, evidence links, and any active clarification.

### Model Change

1. Select another Model Profile in the conversation header when no turn is unfinished.
2. New turns snapshot that profile revision.
3. Prior turns keep their recorded endpoint/model metadata and answer projection.

## Implementation Sequence

1. Split runtime infrastructure config from per-command Model Access Config; add regression tests.
2. Add catalogue migrations, repository module, password hashing, cookie Login Sessions, and credential encryption tests.
3. Add authentication and Model Profile routes with ownership and endpoint validation tests.
4. Add durable conversation and turn routes; replace process-local capability maps for the new interface.
5. Rebuild the frontend around authenticated workspace state, conversation navigation, and Model Profile settings.
6. Add end-to-end tests for two users, restart restoration, ownership isolation, credential non-disclosure, profile revision locking, and full answer replay.
7. Rebuild and deploy the WSL2 Demo, then verify desktop and mobile layouts in a real browser.

## Acceptance Criteria

1. Two accounts cannot list, load, mutate, or execute each other's conversations, turns, or Model Profiles.
2. Reloading the browser or restarting the host retains Login Sessions, conversation lists, prior answers, evidence links, and active clarification state.
3. Creating a new conversation never hides older conversations.
4. API keys never appear in responses, logs, traces, HTML, frontend storage, or plaintext SQLite pages.
5. Model Profile endpoint, model ID, and credential are used for both clarification and research execution of a turn.
6. Editing a Model Profile used by an unfinished turn is rejected with a bounded actionable error.
7. Conversation rename, archive, model selection, and ordered turn replay work on desktop and mobile.
8. L2/L3 trace ownership, field filtering, pagination, and natural dialogue contracts remain green.
9. Rust format, Clippy, all tests, TypeScript check, production build, and browser desktop/mobile checks pass.
10. The deployed Demo is reachable at the reported local URL.
