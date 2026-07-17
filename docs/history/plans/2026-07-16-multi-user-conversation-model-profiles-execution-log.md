# Multi-user Workspace Execution Log

> Historical execution record retained as verification evidence, not a current contract.

Date: 2026-07-16
Status: deployed to WSL2 local Demo

## Decisions Applied

1. The SQLite catalogue owns account identity, login sessions, Model Profile ownership,
   conversation metadata, and turn projections. Append-only core JSONL logs and snapshots remain
   the research audit source.
2. Passwords use Argon2id. Browser login tokens are random, HttpOnly, SameSite=Strict cookies;
   SQLite stores only SHA-256 token hashes.
3. A user Model Profile stores API base URL and model ID in the catalogue. Its API key is
   encrypted with AES-256-GCM and is never returned by API projections.
4. Each Research Turn records the selected Model Profile revision. A profile or conversation cannot
   change while it has an unfinished turn.
5. The WSL deployment creates a persistent, private credential-encryption key on first launch.
   It is intentionally independent from the repository `.env` file so container rebuilds and host
   restarts preserve access to existing encrypted credentials.

## Delivered Scope

- Renamed core modules and domain concepts according to the naming audit and execution plan.
- Added Accounts, Login Sessions, Model Profiles, Research Conversations, Research Turns, and
  SQLite migrations.
- Added authenticated workspace APIs with SQL ownership checks and bounded public errors.
- Added an authenticated browser workspace with conversation navigation, rename/archive, model
  selection, profile management, clarification continuation, and ordered turn replay.
- Added `scripts/verify-demo-workspace.mjs` for HTTP-level account isolation, credential
  non-disclosure, active-turn locking, logout, and host-restart restoration.
- Updated WSL Containerfile and `wsl-demo-up.sh` to include the schema migration and persistent
  credential key lifecycle.

## Verification Evidence

| Gate | Result |
|---|---|
| Core `cargo fmt --all -- --check` | passed |
| Core `cargo clippy --all-targets -- -D warnings` | passed |
| Core `cargo test --all-targets` | 94 passed; 1 external live test intentionally ignored |
| Demo Host formatting and Clippy | passed |
| Demo Host `cargo test --all-targets` | 12 passed |
| Frontend `npm run check` and `npm run build` | passed |
| HTTP workspace verification | passed, including two-user isolation and process restart |
| WSL2 image build | passed |
| WSL2 `GET /api/health` | `ok` from WSL and Windows localhost |
| WSL2 containers | SearXNG, crawl4ai, and Demo Host running |
| WSL2 runtime credential files | env and encryption key mode `0600` |

## Deployment Result

The WSL2 Demo Host is bound to loopback and available from Windows at
`http://127.0.0.1:8080`. It is intentionally a trusted local deployment: secure Cookie transport
is disabled for HTTP, and private model endpoints are allowed so local gateways work. A networked
deployment must use HTTPS, enable `DEMO_SECURE_COOKIES=true`, and leave private model endpoints
disabled unless that network is explicitly trusted.
