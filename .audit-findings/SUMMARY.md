# biTurbo Full-Codebase Audit — Summary

**Deliverable:** 190 GitHub issues filed at https://github.com/biTurboApp/biTurbo/issues,
all prefixed `Bugs/Improvements:` (192 total on the prefix incl. 2 closed probe issues; one filed
issue #207 closed as duplicate of pre-existing #161).

## Method
- Pass 1: 8 parallel deep-audit agents, one per code slice (ingest/io, operations/commands/state,
  memory/recall/consolidate/embed, db/persistence/index/scheduler, mcp/shell, frontend views,
  frontend core, scripts/build/CI). Every file read completely.
- Personal verification pass by the lead agent over all core backend files (state, project, db,
  persistence, recall, embed, index_engine, io, scheduler, consolidate, commands, application,
  tray, lib, error, mcp transport + schemas) and key frontend files; top findings cross-checked
  against dependency sources (turbovec 0.8.0, ignore 0.4.31) and git-tracked state.
- Pass 2: 4 more agents — components deep-dive, dev-harness audit, security/trust-boundary sweep,
  TS<->Rust contract check. Deduped against pass 1 and against ~100 pre-existing UI:/UX: issues
  from earlier sessions (14 exact duplicates dropped).
- Pass 3 (lead): remaining unread code (mcp helpers/schemas, smoke tests, operations tail,
  Projects/Memories views) — 3 additional findings.

## Counts by severity
- **low**: 107
- **medium**: 62
- **high**: 21

## Counts by kind
- **bug**: 129
- **enhancement**: 61

## Counts by slice
- backend-shell: 31
- backend-ingest-io: 30
- backend-core: 25
- frontend-core: 23
- backend-memory: 21
- backend-storage: 15
- frontend-views: 14

## Highest-impact highlights
1. **Data loss / corruption**: non-idempotent decay destroys good memories; dedup can delete the
   active copy of superseded pairs; INSERT OR REPLACE bypasses FTS delete trigger (stale secrets
   stay searchable); uidmap/tvim desync on crash silently corrupts search; merge_pair hard-deletes
   instead of merging content.
2. **Process-killing panics**: assert_eq! dim mismatches + expect() at startup + panic=abort in
   release -> one bad project row (bit_width) or model switch bricks GUI AND every MCP session.
3. **Security**: path traversal via unvalidated project id (file create/delete outside data dir);
   newline injection into .biTurbo marker redirects cross-agent context; export_memories arbitrary
   overwrite; CSP null + unscoped fs capabilities.
4. **Recall correctness**: -inf pad entries dominate typed searches; explain() panics on empty
   type; reranker boosts exceed the entire RRF score range; tokenizer mismatch makes explanations lie.
5. **Graph view broken end-to-end**: absolute-path UIDs never match edge endpoints; worker-ref null
   race drops first layout; Barnes-Hut runs cannot be cancelled.
6. **Protocol/architecture**: MCP server serializes all requests (cancel impossible during ingest);
   notifications answered (spec violation); scheduler never runs in MCP-only sessions; two
   uncoordinated consolidate paths.

## Artifacts
- Raw slice findings: `.audit-findings/*.json` (8 slices + merged.json)
- Issue manifest: `.audit-findings/issues.jsonl` (190 entries)
- Filing log with issue URLs: `.audit-findings/created.log`
