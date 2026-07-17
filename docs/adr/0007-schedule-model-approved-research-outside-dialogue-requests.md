---
status: accepted
date: 2026-07-16
supersedes: "The synchronous Host execution consequence in ADR 0006"
---

# Schedule model-approved research outside dialogue requests

A dialogue request must return after the model has persisted `continue_dialogue` or `start_research`; it must not remain open while a Research Run waits for capacity, searches, crawls, or synthesizes. When the model chooses `start_research`, Demo Host now schedules the Run in a detached task, returns the pending Turn immediately, and lets the workspace poll its owner-protected Conversation until the Turn is terminal. Automatic execution, bounded concurrency, failure terminalization, and restart recovery remain server-owned, so this responsiveness change does not introduce a confirmation or manual start action.

Keeping execution synchronous made the browser look frozen during a real six-minute Run and made client disconnects capable of cancelling an in-flight handler before the catalogue reached a terminal state. Background scheduling trades immediate terminal responses for an explicit `ready | running` public state, but that state is observational only: the user cannot and need not advance it.
