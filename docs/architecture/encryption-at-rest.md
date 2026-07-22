# Encryption at Rest — v0.3 Design Record

Status: design only. v0.3 does not claim or implement encryption at rest.

## Threat model

Future encryption should protect a copied device, user data directory, backup,
SQLite database/WAL, vector index, and transient migration backup. It cannot
protect data from malware running as the logged-in user, an unlocked application,
screen capture, agent-provided plaintext, or a separately logging local model
runtime.

## Recommended direction

Use full-database encryption for SQLite plus authenticated encryption for vector
files, index metadata, and backups. Field-level encryption alone would leave FTS,
indexes, relationships, and access patterns exposed and would complicate local
search. A versioned envelope format should bind project ID, artifact type,
generation, and content digest as authenticated metadata.

Generate one random data-encryption key per installation and wrap it with a key
stored in the platform credential service (macOS Keychain, Windows Credential
Manager/DPAPI, Linux Secret Service). An optional recovery key should wrap the
same data key. Rotation creates a new wrapping key first; full re-encryption is a
separate checkpointed operation.

## Migration and recovery requirements

- Migrate a copy, verify all database/index invariants, then atomically activate.
- Never replace the plaintext source until the encrypted copy is verified.
- Keep updater rollback compatible with both format versions during transition.
- Coordinate GUI and MCP through the existing interprocess lock and operation
  supervisor.
- Ensure crash recovery cannot mix plaintext metadata with encrypted vectors.
- Encrypt backups with independently versioned headers and checksums.
- Treat deleted plaintext, WAL pages, temporary files, and old backups explicitly;
  SQL row deletion alone is not secure erasure.

## Deferred decisions

Library selection, recovery-key UX, Linux key-store fallback, and exact file
format require a separately reviewed security implementation. No production
dependency, key, schema encryption, or marketing claim is introduced in v0.3.

