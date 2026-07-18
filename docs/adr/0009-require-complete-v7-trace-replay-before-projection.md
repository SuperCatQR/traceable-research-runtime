---
status: accepted
date: 2026-07-17
supersedes: "The research trace v6 persistence consequences in ADR 0005 and ADR 0006"
---

# Require complete v7 trace replay before projection

Research Trace v7 persists every event in an envelope with a contiguous `sequence` and a nondecreasing `occurred_at`. The only public read boundary is `replay_trace`, which validates the full file before returning a `ReplayedTrace`. Runtime recovery, resume, and Demo Host L2/L3 projection consume that validated result. Header-only replay and direct deserialization of bare research events are removed.

This prevents a caller from accepting a valid header while overlooking a truncated, reordered, duplicated, post-terminal, or rationale-invalid event later in the file. L1 returns only answer text and necessary sources. L2 is a non-reversible overview that excludes engine attempts and fallback. L3 receives review-safe projections with v7 sequence/time plus typed engine attempts, fallback, and exploration stop reasons; it never receives raw JSONL.

Online compatibility with v1/v2 research traces is deliberately removed. Trace v7 must start in a new deployment storage generation, while earlier generations remain untouched for historical audit. Deployment procedures must not delete, migrate, mount, or append to the old volume.
