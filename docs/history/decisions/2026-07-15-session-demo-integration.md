# Session Demo integration

> Superseded integration record. The authenticated Conversation workspace is governed by ADR 0006.

Date: 2026-07-15
Status: superseded on 2026-07-16

> Historical decision record. The capability-token, unauthenticated session demo described below
> was replaced by the authenticated Conversation workspace. Its dedicated clarification reply,
> retry, manual research routes and user confirmation flow are no longer current behavior; see
> [ADR 0006](../../adr/0006-model-led-dialogue-and-tiered-trace-disclosure.md) and
> [the current architecture](../../web-search-architecture.md).

## Why

The backend now has durable, isolated research sessions, but the demo still starts every top-level question through the compatibility `start_intake(question)` path. The chat window therefore looks conversational while each question actually belongs to a separate private session.

This phase exposes the stable session contract through the localhost-only demo host and makes the minimal frontend reuse one session across successive research questions.

## Current state

- `ResearchService::create_session`, `read_session`, and `start_intake_in_session` are implemented.
- `demo-host` exposes only intake-scoped routes.
- The frontend keeps rendered messages in memory but has no backend session identity.
- The host is loopback-only and intended for a single local user.

## Decision

Add these demo HTTP routes:

- `POST /api/sessions` creates a session and returns a process-local random capability handle plus turn counts.
- `GET /api/sessions/{session_id}` replays a session summary.
- `POST /api/sessions/{session_id}/intakes` starts a new turn in that session.

Keep the existing `/api/intakes` create/reply/retry/research routes for compatibility, but project every backend session and clarification ID to an unguessable process-local capability handle. New reply, retry, and research routes are session-scoped and verify that the clarification capability belongs to the supplied session capability. Browser clients never receive predictable backend persistence IDs.

The host rejects non-loopback `Host` values and cross-origin browser requests. This is defense in depth for a loopback-only, unauthenticated demo; it is not a substitute for authentication in a shared deployment.

The frontend creates one session on startup. Every new top-level question uses that `session_id`; clarification replies continue using the current `clarification_id`. A compact icon button creates a new isolated session and clears the visible transcript after explicit browser confirmation when messages exist.

For this demo phase:

- Session state is memory-only in the browser; reload creates a new session.
- There is no session list, rename, delete, authentication, multi-user ownership, or cross-device restoration.
- A single session permits one unfinished turn, matching the backend invariant.
- The frontend never sends conversation history; the backend loads and freezes it.

## Impacted files

- `demo-host/src/main.rs`
- `demo/src/api.ts`
- `demo/src/main.ts`
- `demo/src/styles.css`
- focused demo-host/frontend tests or checks
- WSL demo image after verification

The core backend under `src/` is not changed in this phase.

## Acceptance criteria

1. The frontend creates a backend session capability before accepting a top-level question.
2. Two successive top-level questions in one browser transcript call the same session-scoped intake route.
3. Clicking New session creates a different session and clears local messages.
4. Different session IDs remain isolated by the existing backend contract.
5. Intake clarification and recoverable Intake retry continue to work. Research errors return a bounded `retryable` flag: transport/setup/non-terminal failures preserve the current turn and allow same-run retry; while such a turn awaits retry, the composer is locked so the user can only retry it or start a new isolated session. Only a session-projected terminal `TurnFailed` asks the user to submit a new turn. Successful research still renders claims and sources.
6. Existing intake-only HTTP clients remain compatible.
7. Invalid session IDs and invalid user text return bounded public errors without leaking internals.
8. TypeScript build, demo-host Rust checks/tests, backend tests, and browser desktop/mobile checks pass.
9. The WSL image is rebuilt and a real two-turn follow-up demonstrates inherited context.
