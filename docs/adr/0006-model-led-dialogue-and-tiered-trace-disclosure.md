---
status: accepted
date: 2026-07-16
supersedes: "Clarification and workspace-exposure details in ADR 0005"
---

# Model-led dialogue and tiered trace disclosure

## Context

The previous demo made the model return `ask | complete`, rendered a dedicated clarification
question flow, and exposed an increasingly large trace directly below each chat turn. That design
made the internal Research Brief feel like a user form, required an explicit progression action,
and placed operational detail in the main conversation even when the user only wanted to discuss
their research question.

The product decision is that the user should only use natural language. The structured Brief is a
model-generated semantic work product: it should constrain the execution safely without becoming a
visible form or forcing the model into a fixed question sequence. Auditability still matters, but
it must not disclose hidden reasoning, prompts, credentials, or whole source bodies.

## Decision

### 1. The model controls dialogue progression

Every evaluation returns exactly one visible `assistant_message`, one internally structured
`ResearchBrief`, a concise review-safe `rationale`, and one of two decisions:

- `continue_dialogue`: persist the reply and await another ordinary user message;
- `start_research`: freeze the model's current Brief and automatically prepare and execute the
  Research Run.

There is no confirmation button, Brief editor, multiple-choice clarification form, question-count
limit, final-forcing prompt, or browser command to start research. The model may express uncertainty
in natural language, but it is not required to use a dedicated question schema. `FrozenResearchBrief`
is an internal immutability and hash-validation type whose `frozen_at` timestamp records when the
model-approved Brief became executable; it does not represent user confirmation.

### 2. The state machine records dialogue and automatic execution

Clarification schema v5 records `intake_started`, `user_message_received`,
`model_understanding`, `run_prepared`, `research_preparation_failed`, `research_run_failed`, `cancelled`, and `intake_failed`. It preserves the full
natural dialogue, the model-owned Brief version, its content hash, and the concise decision
rationale. `ResearchReady` is a model outcome, not a user-action state; Demo Host immediately
persists it, returns the pending Turn, and schedules preparation and execution outside the
dialogue request while holding the configured research-concurrency permit.

Conversation schema v2 and Clarification schema v5 deliberately do not read or append their prior
wire formats, and Trace schema v6 does not read the v5 Brief wire format. Deployments must use a
new runtime data directory or persistent volume. This decision does not authorize silent deletion
of the old data; operators retain it separately when it is needed for historical audit.

### 3. Trace disclosure has three levels

| Level | Surface | Contract |
| --- | --- | --- |
| L1 | Main chat | Natural dialogue, research status, final answer, and necessary sources only. |
| L2 | Right-side Research Overview | Server-projected compact understanding, coverage, source and synthesis summary. |
| L3 | Right-side Audit Details | Owner-authorized, stage-filtered, paginated review-safe audit entries. |

L2 is not a client-side filter over raw JSONL. L3 is not a chain-of-thought endpoint. Both levels
exclude system prompts, hidden reasoning, API keys, raw model inputs, and full snapshot bodies.
Reasons are concise review summaries linked to observable inputs, evidence gaps, source choices,
or final synthesis. The public workspace contract is:

```text
POST /api/conversations/{conversation_id}/turns
POST /api/conversations/{conversation_id}/turns/{turn_id}/messages
GET  /api/conversations/{conversation_id}/turns/{turn_id}/trace/summary
GET  /api/conversations/{conversation_id}/turns/{turn_id}/trace/audit?stage=&cursor=&limit=
```

All turn and trace access checks ownership through the authenticated account. Other users receive
`404`, while unauthenticated callers receive `401`.

## Consequences

- The primary workspace remains a natural chat rather than a workflow form. A user cannot be
  blocked on understanding a system-specific Brief representation.
- The Demo chat fixes the default web-first answer style rather than adding a second input control;
  alternative execution styles remain a host-level capability, not part of the natural dialogue
  path.
- The model has flexibility to decide whether more dialogue is useful, while programmatic state,
  schema, hash and resource checks keep execution bounded and replayable.
- A model-approved run cannot be stranded in a state that requires a second browser action. Per
  [ADR 0007](0007-schedule-model-approved-research-outside-dialogue-requests.md), the Host returns
  the pending Turn immediately and owns background execution, terminalization, and restart recovery.
- Reviewers can inspect progressively more detail without turning normal chat into an audit
  timeline or receiving protected model internals.
- ADR 0005's principle that rationales are review-safe rather than hidden reasoning remains in
  force. Its old `ask | complete`, single trace endpoint and inline-timeline details are
  superseded by this ADR.
