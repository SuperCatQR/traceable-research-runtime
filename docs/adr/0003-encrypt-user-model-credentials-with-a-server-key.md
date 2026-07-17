---
status: accepted
---

# Encrypt user model credentials with a server key

Hash passwords with Argon2id, store only Login Session token hashes, and encrypt Model Profile API keys with AES-256-GCM under a deployment-owned `DEMO_CREDENTIAL_ENCRYPTION_KEY`. OS keyrings were rejected because the WSL/container deployment needs deterministic non-interactive startup, and plaintext SQLite storage was rejected because a copied catalogue must not immediately reveal user credentials; losing the server key makes stored API keys unrecoverable by design.
