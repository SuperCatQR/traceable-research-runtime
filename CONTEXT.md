# Traceable Research Runtime

This context names the durable research workflow exposed by the runtime and the demo host. The vocabulary separates a user's long-lived conversation from clarification and one executable research run.

## Conversation

**Research Conversation**:
A user-owned, durable sequence of related research turns whose completed answers provide intent context to later turns.
_Avoid_: Research Session, chat session, thread

**Research Turn**:
One ordered user question in a Research Conversation, including clarification, execution, and its terminal outcome.
_Avoid_: Intake, request, job

**Clarification**:
The model-led state machine that turns a user's question and conversation history into a validated Research Brief.
_Avoid_: Intake Session, preflight

**Research Brief**:
The model-generated normalized question, scope, source constraints, output expectations, and accepted assumptions frozen after the model decides to start research. The freeze is an internal integrity step, not a user confirmation.
_Avoid_: Prompt, spec

## Execution

**Research Run**:
One traceable execution of an internally frozen Research Brief through query generation, web search, snapshot capture, evidence selection, and grounded synthesis.
_Avoid_: Research Session, pipeline

**Web Snapshot**:
An immutable, content-addressed capture of a fetched public web page used as research evidence.
_Avoid_: Page, document, crawl result

**Grounded Claim**:
A verifiable statement in the final answer linked to one or more Web Snapshots.
_Avoid_: Fact, citation

**Search Provider Attempt**:
One explicit request to the configured Brave Search API for one query, with a typed completed, unavailable, or contract-rejected outcome. Legacy Google/Bing attempts remain readable only for v7 Trace replay compatibility.
_Avoid_: Search retry, provider call

**Exploration Stop Reason**:
The single persisted reason a Research Run stops generating new search rounds: planned rounds completed, input budget reached, snapshot limit reached, or no new URLs found.
_Avoid_: Search error, completion status

## Trace Disclosure

**Research Trace**:
An append-only v7 audit record of how one Research Turn was understood, prepared, researched, and completed or failed. Every research event has a stable sequence and occurrence time.
_Avoid_: Debug log, chain-of-thought

**Research Overview**:
A compact, review-safe account of one Research Turn for a user who wants to understand coverage, sources, and outcome without reading every event.
_Avoid_: Raw trace, event dump

**Audit Detail**:
A paginated, review-safe account of individual Research Trace events for diagnosis and review.
_Avoid_: Hidden reasoning, model transcript

## Identity And Models

**User Account**:
The durable owner of Research Conversations and Model Profiles.
_Avoid_: User record, tenant

**Login Session**:
A revocable authenticated browser session belonging to one User Account.
_Avoid_: Session, token

**Model Profile**:
A user-owned OpenAI-compatible endpoint, model identifier, display name, and encrypted credential used for clarification and research generation.
_Avoid_: Model config, provider
