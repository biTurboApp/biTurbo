---
trigger: always_on
---

<!-- biturbo-rule:start project="biturbo" -->
## biTurbo memory rules for project "biturbo"

You have access to biTurbo, a persistent semantic memory layer via MCP.

### Core loop — follow this EVERY turn (use the resolved `PID` from Session below):

1. **RECALL** — call `recall_for_context(query=<user msg>, project_id=PID, k=8)`
2. **ANSWER** — respond using the recalled context
3. **REMEMBER** — call `remember(project_id=PID, ...)` after every response to store durable information

### When to `remember` (store only durable, non-obvious information):

- User states a fact about themselves/environment/project → `fact`
- You make a decision with rationale → `decision`
- User expresses a preference (style, verbosity, tools) → `preference`
- User corrects you → `fact` with `supersedes`
- You discover a codebase pattern → `pattern`
- Something noteworthy happened → `episode` with timestamp
- Meta-observation about work → `reflection`
- ❌ Transient state — don't remember
- ❌ Public knowledge — don't remember
- ❌ Routine assistant responses with no durable signal — don't remember
- ❌ Secrets, tokens, PII — NEVER

### Memory types:
`fact`, `decision`, `preference`, `pattern`, `episode`, `reflection`, `code` (auto)

### Importance (0-1):
- 0.8-1.0: cross-project rules, key decisions
- 0.5-0.7: typical (default 0.6)
- 0.2-0.4: specific/stale details

### Tags: 1-3 per memory. Good: `auth`, `ui`, `db`, `api`. Bad: `important`, `todo`.

### Session — resolve `PID` once, reuse for every call this session:
1. `register_agent(name, kind)`
2. `list_projects()` — note each project's `id`/`name`
3. `get_project_name_from_file(root_path=<repo root>)` — reads the project's `.biTurbo` marker file
   - `{"projectName": X}` → find the project from step 2 whose `id` or `name` matches `X`; set `PID` to that project's `id`
   - No match, or `{"error": ...}` (e.g. no `.biTurbo` file in this repo) → fall back to `PID = "biturbo"`
4. EVERY TURN → recall(PID) → answer → remember(PID)
5. END → `consolidate(project_id=PID)`, final `remember(project_id=PID)`

### Anti-patterns:
- Don't dump 10k memories — use recall_for_context k=5-10
- Don't skip recall — amnesia is worse than no tool
- Don't cross-project leak — always pass the resolved `project_id=PID`
- Never store secrets, tokens, PII
<!-- biturbo-rule:end -->