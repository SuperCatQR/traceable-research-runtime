# Rename Plan

> Historical planning record retained for review. `CONTEXT.md` defines the current ubiquitous language.

Date: 2026-07-16
Status: accepted for staged execution

## Constraints

1. Preserve every pre-existing worktree change.
2. Keep persisted JSON event tags and fields readable without migration.
3. Keep SQLite snapshot schema compatible.
4. Keep current HTTP routes working until replacement conversation routes pass end-to-end tests.
5. Compile and test after each batch so a failing rename has a small search surface.
6. Do not add a naming linter or rename familiar, locally unambiguous terms.

## Batch 1: Domain Vocabulary

- Rename the conversation, clarification, and research-run types that currently collide on `session` and `intake`.
- Rename domain fields such as `turn` to `turn_number` in code while preserving serialized names with Serde attributes.
- Rename ambiguous domain values such as `Query`, `Answer`, and `Excerpt`.
- Update tests and crate-root exports.
- Gate: core format, Clippy, and all-target tests.

## Batch 2: Runtime And Adapters

- Rename `AppConfig`, `ResearchService`, `PreparedRun`, and the public execution commands.
- Rename concrete external adapters by protocol and role.
- Rename research backend operations by their object and outcome.
- Rename module files to domain concepts and update documentation links.
- Gate: core format, Clippy, and all-target tests.

## Batch 3: Demo Transport

- Rename capability maps, request/response types, handlers, and projection functions.
- Preserve legacy route behavior during this batch.
- Make public capability tokens and internal persistence IDs impossible to confuse in local names.
- Gate: demo-host format, Clippy, and tests.

## Batch 4: Frontend

- Rename the transport module and domain response types.
- Rename active-conversation state and workflow commands.
- Split rendering only where the upcoming session manager needs a real module seam.
- Gate: TypeScript check and production build.

## Batch 5: Contract And Documentation Sweep

- Search for every retired identifier in source, tests, docs, routes, and environment templates.
- Retain only explicit compatibility aliases or historical decision text.
- Update README and architecture documentation to the domain vocabulary in `CONTEXT.md`.
- Run the complete baseline suite and record the result.

## Compatibility Strategy

Code identifiers may change immediately because this repository controls all current callers. Durable formats change only through versioned readers or aliases. Replacement HTTP routes will be introduced alongside existing demo routes, exercised by the new frontend, and the old routes will be marked compatibility-only before later removal.

## Rollback Strategy

Each batch is independently compilable. A batch that fails its gate is corrected in place before the next begins; existing user changes are never reverted. Persisted data does not need rollback because this phase does not rewrite event tags, JSON field names, or SQLite columns.
