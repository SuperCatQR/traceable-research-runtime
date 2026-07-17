# Record auditable decision rationales without hidden reasoning

- Status: superseded in part by [ADR 0006](0006-model-led-dialogue-and-tiered-trace-disclosure.md)
- Date: 2026-07-16

## Context

Append-only JSONL traces already capture state transitions and Web evidence, but a decision record
without a reason is hard to review. In particular, the former clarification lifecycle showed a
model question or completed Brief without saying why.

Persisting hidden chain-of-thought is neither necessary nor appropriate for this product. The
audit requirement is a concise, user-reviewable justification tied to observable inputs and
evidence.

## Decision

Every model-controlled decision must persist a concise decision rationale in an owned trace:

| Phase | Trace field |
| --- | --- |
| Clarification `continue_dialogue` / `start_research` | `model_understanding.rationale` |
| Knowledge-only draft | `knowledge_draft.basis_summary` |
| Search-query planning | `query.gap` |
| Snapshot selection | `snapshot_selection.selected[].reason` |
| Final claim acceptance | `claim.rationale` |
| Weighted final composition | `answer.comparison.synthesis_rationale` |

Conversation schema v2, Clarification event schema v5, and research trace schema v6 make this a persistence contract:
new writes and new-format replay reject a missing, too-short, or overlong rationale. A
Each log keeps one schema version for its lifetime; Conversation v2 and Clarification v5
intentionally do not replay prior session/intake logs. Trace v6 does not replay the v5
`confirmed_at` Brief wire format. Older v1/v2 research traces can remain distinguishable as
`legacy_unverified` rather than retroactively asserted to satisfy the new contract.

The authenticated workspace exposes a turn-scoped L2 summary and L3 paginated audit endpoint to
the owning account. The normal chat surface carries only the natural assistant message; the right
sidebar is responsible for the review-safe rationale fields. ADR 0006 defines those disclosure
levels and replaces the former inline decision timeline.

## Consequences

- Reviewers can explain each externally meaningful model decision without receiving hidden
  reasoning or secrets.
- `required_and_validated` means the persisted schema enforced the rationale contract; it is not
  a claim that the model's conclusion is factually correct.
- The trace remains append-only and replayable; it is not a cryptographic tamper-evidence system.
- One old trace may lack a new rationale field. New runs are required to include it.
