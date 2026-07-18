# Contain long answers and do not cache the workspace entry

Date: 2026-07-16

## Problem

At ordinary desktop heights, a long research answer could apply its automatic minimum height to
the workspace grid item. The workspace then grew beyond `100dvh`, placed the composer below the
viewport, and left the transcript without its own scroll range. A separately observed old rich
answer view also showed that the HTML entry response had no explicit cache policy, so a browser
could keep an entry that referenced an obsolete hashed bundle after a deployment.

## Decision

- The research workspace is allowed to shrink inside the fixed-height shell with
  `min-height: 0`; overflow stays inside the transcript, whose scrollbar reserves a stable gutter.
- Every HTML response, including the not-found HTML fallback, uses `Cache-Control: no-store`.
- Successful `/assets/` responses use `Cache-Control: public, max-age=31536000, immutable` because
  Vite gives those files content hashes.

## Reasoning

The header, transcript, and composer are three persistent workspace regions. Only the transcript
may grow, so it must own scrolling while the composer remains reachable. HTML cannot be cached
across deployments because it selects the active bundle; hashed assets are immutable by identity
and can be cached aggressively without serving stale code.

## Verification

- The layout fixture measures a 2048 by 1029 viewport with an 80-paragraph answer and requires the
  composer to remain fully visible while the transcript has a scroll range.
- The integration verification used when accepting this decision asserted the cache policy on both
  `/` and its referenced hashed asset through the real Demo Host.
