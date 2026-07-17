---
status: accepted
---

# Separate host catalogue from research audit storage

Store User Accounts, Login Sessions, Model Profiles, conversation ownership, and turn projections in a demo-host SQLite catalogue while keeping the core runtime's append-only JSONL logs, trace files, and immutable Web Snapshots as the research audit store. A single relational database would simplify joins but would couple identity lifecycle to the core library's replay contracts; the split keeps ownership queries transactional without rewriting or weakening existing audit data.
