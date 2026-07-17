---
status: accepted
superseded_in_part_by: "ADR 0006"
---

# Separate conversation, clarification, and research run language

Use **Research Conversation**, **Research Turn**, **Clarification**, and **Research Run** as distinct code concepts because the previous word `session` referred to three different lifecycles. The vocabulary decision remains current. Its compatibility commitment for prior wire formats and HTTP routes was superseded by ADR 0006, which intentionally starts the current demo on new Conversation, Clarification, and Trace schemas while retaining old storage separately for audit.
