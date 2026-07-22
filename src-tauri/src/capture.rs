//! Privacy-preserving observation intake and candidate review.

use crate::db::log_activity;
use crate::error::{BiError, BiResult};
use crate::state::AppState;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_OBSERVATION_BYTES: usize = 256 * 1024;
const DEFAULT_EVIDENCE_CHARS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitObservationInput {
    pub project_id: String,
    pub source_kind: String,
    pub external_id: String,
    pub session_id: Option<String>,
    pub occurred_at: i64,
    pub role: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitObservationResult {
    pub accepted: bool,
    pub duplicate: bool,
    pub observation_id: String,
    pub candidate_ids: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub id: String,
    pub observation_id: String,
    pub project_id: String,
    pub content: String,
    pub mem_type: String,
    pub tags: Vec<String>,
    pub confidence: f32,
    pub status: String,
    pub duplicate_memory_uid: Option<String>,
    pub contradiction_uid: Option<String>,
    pub resulting_memory_uid: Option<String>,
    pub version: i64,
    pub evidence: Vec<CandidateEvidence>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEvidence {
    pub excerpt: String,
    pub source_pointer: Option<String>,
    pub source_timestamp: i64,
    pub evidence_hash: String,
    pub extraction_method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateDecisionInput {
    pub candidate_id: String,
    pub action: String,
    pub edited_content: Option<String>,
    pub target_memory_uid: Option<String>,
    pub expected_version: i64,
    pub decided_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateDecisionResult {
    pub candidate: MemoryCandidate,
    pub decision_id: String,
    pub memory_uid: Option<String>,
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturePolicy {
    pub project_id: String,
    pub enabled_sources: Vec<String>,
    pub allowed_categories: Vec<String>,
    pub extraction_mode: String,
    pub ollama_endpoint: String,
    pub ollama_model: Option<String>,
    pub approval_mode: String,
    pub auto_approve_categories: Vec<String>,
    pub evidence_max_chars: usize,
    pub redaction_mode: String,
    pub notify_candidates: bool,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationSource {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub name: String,
    pub root_path: Option<String>,
    pub enabled: bool,
    pub config: serde_json::Value,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
    pub processed_count: i64,
    pub candidate_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
struct ExtractedCandidate {
    content: String,
    mem_type: String,
    tags: Vec<String>,
    confidence: f32,
}

pub fn submit_observation(
    state: &AppState,
    input: SubmitObservationInput,
) -> BiResult<SubmitObservationResult> {
    crate::project::get(state, &input.project_id)?;
    validate_source_kind(&input.source_kind)?;
    if input.external_id.trim().is_empty() {
        return Err(BiError::Invalid("external_id is required".into()));
    }
    if input.content.trim().is_empty() {
        return Err(BiError::Invalid("observation content is empty".into()));
    }
    if input.content.len() > MAX_OBSERVATION_BYTES {
        return Err(BiError::Invalid(format!(
            "observation exceeds {MAX_OBSERVATION_BYTES} bytes"
        )));
    }
    let content_hash = hash_text(&input.content);
    if let Some((observation_id, prior_hash)) = find_receipt(
        state,
        &input.project_id,
        &input.source_kind,
        &input.external_id,
    )? {
        if prior_hash != content_hash {
            return Err(BiError::Invalid(
                "external_id was already used with different content".into(),
            ));
        }
        return Ok(SubmitObservationResult {
            accepted: true,
            duplicate: true,
            candidate_ids: candidate_ids_for_observation(state, &observation_id)?,
            observation_id,
            warnings: Vec::new(),
        });
    }

    let policy = get_capture_policy(state, &input.project_id)?;
    if !policy
        .enabled_sources
        .iter()
        .any(|kind| kind == &input.source_kind)
    {
        return Err(BiError::Invalid(format!(
            "source '{}' is disabled by the project capture policy",
            input.source_kind
        )));
    }

    let (redacted, secret_count) = redact_secrets(&input.content);
    let mut warnings = Vec::new();
    if secret_count > 0 {
        warnings.push(format!("redacted {secret_count} potential secret(s)"));
    }
    let extracted = extract_deterministic(&redacted, &input.role, &policy);
    let observation_id = format!("obs-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().timestamp_millis();
    let occurred_at = if input.occurred_at > 0 {
        input.occurred_at
    } else {
        now
    };
    let source_pointer = input
        .metadata
        .as_ref()
        .and_then(|value| value.get("source_pointer"))
        .and_then(serde_json::Value::as_str)
        .map(String::from);
    let evidence_excerpt = bounded_chars(&redacted, policy.evidence_max_chars);
    let evidence_hash = hash_text(&evidence_excerpt);
    let mut candidate_ids = Vec::with_capacity(extracted.len());

    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO observations(id, project_id, source_kind, external_id, session_id,
                 occurred_at, role, content_hash, status, candidate_count, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'processed',?9,?10)",
            rusqlite::params![
                observation_id,
                input.project_id,
                input.source_kind,
                input.external_id,
                input.session_id,
                occurred_at,
                input.role,
                content_hash,
                extracted.len() as i64,
                now,
            ],
        )?;
        for extracted in &extracted {
            let candidate_id = format!("candidate-{}", uuid::Uuid::new_v4());
            let exact_duplicate: Option<String> = tx
                .query_row(
                    "SELECT uid FROM memories WHERE project_id = ?1 AND lower(trim(content)) = lower(trim(?2)) LIMIT 1",
                    rusqlite::params![input.project_id, extracted.content],
                    |row| row.get(0),
                )
                .optional()?;
            let pending_duplicate: Option<String> = tx
                .query_row(
                    "SELECT id FROM memory_candidates WHERE project_id = ?1 AND status = 'pending'
                     AND lower(trim(content)) = lower(trim(?2)) LIMIT 1",
                    rusqlite::params![input.project_id, extracted.content],
                    |row| row.get(0),
                )
                .optional()?;
            if exact_duplicate.is_some() || pending_duplicate.is_some() {
                continue;
            }
            let contradiction_uid = find_likely_contradiction(tx, &input.project_id, extracted)?;
            tx.execute(
                "INSERT INTO memory_candidates(id, observation_id, project_id, content, mem_type,
                     tags, confidence, status, duplicate_memory_uid, contradiction_uid, created_at, updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,'pending',?8,?9,?10,?10)",
                rusqlite::params![
                    candidate_id,
                    observation_id,
                    input.project_id,
                    extracted.content,
                    extracted.mem_type,
                    serde_json::to_string(&extracted.tags)?,
                    extracted.confidence,
                    exact_duplicate,
                    contradiction_uid,
                    now,
                ],
            )?;
            tx.execute(
                "INSERT INTO candidate_evidence(candidate_id, excerpt, source_pointer,
                     source_timestamp, evidence_hash, extraction_method)
                 VALUES(?1,?2,?3,?4,?5,'deterministic-v1')",
                rusqlite::params![
                    candidate_id,
                    evidence_excerpt,
                    source_pointer,
                    occurred_at,
                    evidence_hash,
                ],
            )?;
            candidate_ids.push(candidate_id);
        }
        tx.execute(
            "UPDATE observations SET candidate_count = ?1 WHERE id = ?2",
            rusqlite::params![candidate_ids.len() as i64, observation_id],
        )?;
        log_activity(
            tx,
            Some(&input.project_id),
            None,
            "capture",
            None,
            Some(&serde_json::json!({
                "observation_id": observation_id,
                "source_kind": input.source_kind,
                "candidate_count": candidate_ids.len()
            })),
        )?;
        Ok(())
    })?;

    Ok(SubmitObservationResult {
        accepted: true,
        duplicate: false,
        observation_id,
        candidate_ids,
        warnings,
    })
}

pub fn list_candidates(
    state: &AppState,
    project_id: Option<&str>,
    status: Option<&str>,
    limit: usize,
    offset: usize,
) -> BiResult<Vec<MemoryCandidate>> {
    let status = status.unwrap_or("pending");
    if !matches!(
        status,
        "pending" | "approved" | "rejected" | "merged" | "expired" | "processing_error" | "all"
    ) {
        return Err(BiError::Invalid(format!(
            "unknown candidate status {status}"
        )));
    }
    let conn = state.db.conn()?;
    let mut sql = String::from(
        "SELECT id, observation_id, project_id, content, mem_type, tags, confidence, status,
                duplicate_memory_uid, contradiction_uid, resulting_memory_uid, version, created_at, updated_at
         FROM memory_candidates WHERE 1=1",
    );
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(project_id) = project_id {
        sql.push_str(" AND project_id = ?");
        values.push(project_id.to_string().into());
    }
    if status != "all" {
        sql.push_str(" AND status = ?");
        values.push(status.to_string().into());
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ? OFFSET ?");
    values.push((limit.clamp(1, 500) as i64).into());
    values.push((offset as i64).into());
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), row_to_candidate_base)?;
    let mut candidates: Vec<MemoryCandidate> = rows.filter_map(Result::ok).collect();
    for candidate in &mut candidates {
        candidate.evidence = evidence_for(&conn, &candidate.id)?;
    }
    Ok(candidates)
}

pub fn get_candidate(state: &AppState, id: &str) -> BiResult<MemoryCandidate> {
    let conn = state.db.conn()?;
    let mut candidate = conn
        .query_row(
            "SELECT id, observation_id, project_id, content, mem_type, tags, confidence, status,
                    duplicate_memory_uid, contradiction_uid, resulting_memory_uid, version, created_at, updated_at
             FROM memory_candidates WHERE id = ?1",
            rusqlite::params![id],
            row_to_candidate_base,
        )
        .optional()?
        .ok_or_else(|| BiError::NotFound(format!("candidate {id}")))?;
    candidate.evidence = evidence_for(&conn, id)?;
    Ok(candidate)
}

pub fn review_candidate(
    state: &AppState,
    input: CandidateDecisionInput,
) -> BiResult<CandidateDecisionResult> {
    if !matches!(
        input.action.as_str(),
        "approve" | "edit_and_approve" | "reject" | "merge" | "defer"
    ) {
        return Err(BiError::Invalid("unknown candidate decision".into()));
    }
    let candidate = get_candidate(state, &input.candidate_id)?;
    if candidate.status != "pending" {
        let conn = state.db.conn()?;
        let prior: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT id, resulting_memory_uid FROM candidate_decisions
                 WHERE candidate_id = ?1 AND expected_version = ?2",
                rusqlite::params![input.candidate_id, input.expected_version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((decision_id, memory_uid)) = prior {
            return Ok(CandidateDecisionResult {
                candidate,
                decision_id,
                memory_uid,
                operation_id: None,
            });
        }
        return Err(BiError::Invalid("candidate is no longer pending".into()));
    }
    if candidate.version != input.expected_version {
        return Err(BiError::Invalid(format!(
            "candidate version changed; expected {}, actual {}",
            input.expected_version, candidate.version
        )));
    }
    if candidate.contradiction_uid.is_some()
        && input.action == "approve"
        && input.decided_by.as_deref() == Some("automatic")
    {
        return Err(BiError::Invalid(
            "contradictory candidates require explicit review".into(),
        ));
    }
    let content = match input.action.as_str() {
        "edit_and_approve" => input
            .edited_content
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| BiError::Invalid("edited_content is required".into()))?
            .to_string(),
        _ => candidate.content.clone(),
    };
    let decision_id = format!("decision-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().timestamp_millis();
    let mut memory_uid: Option<String> = None;

    state.db.write(|tx| {
        let current: (String, i64) = tx.query_row(
            "SELECT status, version FROM memory_candidates WHERE id = ?1",
            rusqlite::params![input.candidate_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if current.0 != "pending" || current.1 != input.expected_version {
            return Err(BiError::Invalid("candidate changed during review".into()));
        }
        let next_status = match input.action.as_str() {
            "approve" | "edit_and_approve" => "approved",
            "reject" => "rejected",
            "merge" => "merged",
            "defer" => "pending",
            _ => unreachable!(),
        };
        if matches!(input.action.as_str(), "approve" | "edit_and_approve") {
            let uid = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO memories(uid, project_id, mem_type, content, tags, source_agent,
                     importance, created_at, updated_at, last_access, access_count)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,?8,0)",
                rusqlite::params![
                    uid,
                    candidate.project_id,
                    candidate.mem_type,
                    content,
                    serde_json::to_string(&candidate.tags)?,
                    input.decided_by.as_deref().unwrap_or("capture-review"),
                    candidate.confidence.clamp(0.4, 0.9),
                    now,
                ],
            )?;
            tx.execute(
                "UPDATE projects SET memory_count = memory_count + 1, updated_at = ?1 WHERE id = ?2",
                rusqlite::params![now, candidate.project_id],
            )?;
            crate::persistence::queue_index_upsert(tx, &candidate.project_id, &uid, &content)?;
            log_activity(
                tx,
                Some(&candidate.project_id),
                input.decided_by.as_deref(),
                "candidate_approve",
                Some(&uid),
                Some(&serde_json::json!({"candidate_id": input.candidate_id})),
            )?;
            memory_uid = Some(uid);
        } else if input.action == "merge" {
            let target = input
                .target_memory_uid
                .as_deref()
                .ok_or_else(|| BiError::Invalid("target_memory_uid is required".into()))?;
            let exists: i64 = tx.query_row(
                "SELECT COUNT(*) FROM memories WHERE uid = ?1 AND project_id = ?2",
                rusqlite::params![target, candidate.project_id],
                |row| row.get(0),
            )?;
            if exists == 0 {
                return Err(BiError::NotFound(format!("memory {target}")));
            }
            memory_uid = Some(target.to_string());
        }
        tx.execute(
            "INSERT INTO candidate_decisions(id, candidate_id, action, decided_by,
                 edited_content, target_memory_uid, resulting_memory_uid, expected_version, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                decision_id,
                input.candidate_id,
                input.action,
                input.decided_by,
                input.edited_content,
                input.target_memory_uid,
                memory_uid,
                input.expected_version,
                now,
            ],
        )?;
        tx.execute(
            "UPDATE memory_candidates SET status = ?1, resulting_memory_uid = ?2,
                 version = version + 1, updated_at = ?3 WHERE id = ?4",
            rusqlite::params![next_status, memory_uid, now, input.candidate_id],
        )?;
        if input.action == "reject" {
            tx.execute(
                "UPDATE candidate_evidence SET excerpt = '[discarded after rejection]'
                 WHERE candidate_id = ?1",
                rusqlite::params![input.candidate_id],
            )?;
        }
        Ok(())
    })?;
    if memory_uid.is_some() && matches!(input.action.as_str(), "approve" | "edit_and_approve") {
        state.replay_index_mutations(&candidate.project_id)?;
    }
    Ok(CandidateDecisionResult {
        candidate: get_candidate(state, &input.candidate_id)?,
        decision_id,
        memory_uid,
        operation_id: None,
    })
}

pub fn get_capture_policy(state: &AppState, project_id: &str) -> BiResult<CapturePolicy> {
    crate::project::get(state, project_id)?;
    let conn = state.db.conn()?;
    let policy = conn
        .query_row(
            "SELECT project_id, enabled_sources, allowed_categories, extraction_mode,
                    ollama_endpoint, ollama_model, approval_mode, auto_approve_categories,
                    evidence_max_chars, redaction_mode, notify_candidates, updated_at
             FROM capture_policies WHERE project_id = ?1",
            rusqlite::params![project_id],
            row_to_capture_policy,
        )
        .optional()?;
    Ok(policy.unwrap_or_else(|| default_capture_policy(project_id)))
}

pub fn update_capture_policy(
    state: &AppState,
    mut policy: CapturePolicy,
) -> BiResult<CapturePolicy> {
    crate::project::get(state, &policy.project_id)?;
    if !matches!(policy.extraction_mode.as_str(), "deterministic" | "ollama") {
        return Err(BiError::Invalid(
            "extraction_mode must be deterministic or ollama".into(),
        ));
    }
    if !matches!(
        policy.approval_mode.as_str(),
        "review_required" | "trusted_categories"
    ) {
        return Err(BiError::Invalid("invalid approval_mode".into()));
    }
    if policy.extraction_mode == "ollama" && !is_loopback_endpoint(&policy.ollama_endpoint) {
        return Err(BiError::Invalid(
            "Ollama endpoint must be loopback-only".into(),
        ));
    }
    for source in &policy.enabled_sources {
        validate_source_kind(source)?;
    }
    policy.evidence_max_chars = policy.evidence_max_chars.clamp(100, 2_000);
    policy.updated_at = chrono::Utc::now().timestamp_millis();
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO capture_policies(project_id, enabled_sources, allowed_categories,
                 extraction_mode, ollama_endpoint, ollama_model, approval_mode,
                 auto_approve_categories, evidence_max_chars, redaction_mode,
                 notify_candidates, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(project_id) DO UPDATE SET
                 enabled_sources=excluded.enabled_sources,
                 allowed_categories=excluded.allowed_categories,
                 extraction_mode=excluded.extraction_mode,
                 ollama_endpoint=excluded.ollama_endpoint,
                 ollama_model=excluded.ollama_model,
                 approval_mode=excluded.approval_mode,
                 auto_approve_categories=excluded.auto_approve_categories,
                 evidence_max_chars=excluded.evidence_max_chars,
                 redaction_mode=excluded.redaction_mode,
                 notify_candidates=excluded.notify_candidates,
                 updated_at=excluded.updated_at",
            rusqlite::params![
                policy.project_id,
                serde_json::to_string(&policy.enabled_sources)?,
                serde_json::to_string(&policy.allowed_categories)?,
                policy.extraction_mode,
                policy.ollama_endpoint,
                policy.ollama_model,
                policy.approval_mode,
                serde_json::to_string(&policy.auto_approve_categories)?,
                policy.evidence_max_chars as i64,
                policy.redaction_mode,
                policy.notify_candidates as i64,
                policy.updated_at,
            ],
        )?;
        Ok(())
    })?;
    get_capture_policy(state, &policy.project_id)
}

pub fn list_sources(
    state: &AppState,
    project_id: Option<&str>,
) -> BiResult<Vec<ObservationSource>> {
    let conn = state.db.conn()?;
    let mut sql = String::from(
        "SELECT id, project_id, kind, name, root_path, enabled, config, last_sync_at,
                last_error, processed_count, candidate_count, created_at, updated_at
         FROM observation_sources",
    );
    let mut values = Vec::new();
    if let Some(project_id) = project_id {
        sql.push_str(" WHERE project_id = ?1");
        values.push(project_id);
    }
    sql.push_str(" ORDER BY project_id, kind, name");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| {
        let config: String = row.get(6)?;
        Ok(ObservationSource {
            id: row.get(0)?,
            project_id: row.get(1)?,
            kind: row.get(2)?,
            name: row.get(3)?,
            root_path: row.get(4)?,
            enabled: row.get::<_, i64>(5)? != 0,
            config: serde_json::from_str(&config).unwrap_or_else(|_| serde_json::json!({})),
            last_sync_at: row.get(7)?,
            last_error: row.get(8)?,
            processed_count: row.get(9)?,
            candidate_count: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn upsert_source(
    state: &AppState,
    mut source: ObservationSource,
) -> BiResult<ObservationSource> {
    crate::project::get(state, &source.project_id)?;
    validate_source_kind(&source.kind)?;
    if source.kind != "generic" {
        let root = source
            .root_path
            .as_deref()
            .ok_or_else(|| BiError::Invalid("transcript source requires root_path".into()))?;
        if !std::path::Path::new(root).is_dir() {
            return Err(BiError::Invalid(format!(
                "source directory '{root}' does not exist"
            )));
        }
    }
    let now = chrono::Utc::now().timestamp_millis();
    if source.id.is_empty() {
        source.id = format!("source-{}", uuid::Uuid::new_v4());
        source.created_at = now;
    }
    source.updated_at = now;
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO observation_sources(id, project_id, kind, name, root_path, enabled,
                 config, last_sync_at, last_error, processed_count, candidate_count, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, root_path=excluded.root_path,
                 enabled=excluded.enabled, config=excluded.config, updated_at=excluded.updated_at",
            rusqlite::params![
                source.id,
                source.project_id,
                source.kind,
                source.name,
                source.root_path,
                source.enabled as i64,
                source.config.to_string(),
                source.last_sync_at,
                source.last_error,
                source.processed_count,
                source.candidate_count,
                source.created_at,
                source.updated_at,
            ],
        )?;
        Ok(())
    })?;
    list_sources(state, Some(&source.project_id))?
        .into_iter()
        .find(|item| item.id == source.id)
        .ok_or_else(|| BiError::Internal("source missing after upsert".into()))
}

fn default_capture_policy(project_id: &str) -> CapturePolicy {
    CapturePolicy {
        project_id: project_id.to_string(),
        enabled_sources: vec!["generic".into()],
        allowed_categories: vec![
            "preference".into(),
            "decision".into(),
            "fact".into(),
            "pattern".into(),
        ],
        extraction_mode: "deterministic".into(),
        ollama_endpoint: "http://127.0.0.1:11434".into(),
        ollama_model: None,
        approval_mode: "review_required".into(),
        auto_approve_categories: Vec::new(),
        evidence_max_chars: DEFAULT_EVIDENCE_CHARS,
        redaction_mode: "strict".into(),
        notify_candidates: true,
        updated_at: 0,
    }
}

fn extract_deterministic(
    content: &str,
    role: &str,
    policy: &CapturePolicy,
) -> Vec<ExtractedCandidate> {
    if role.eq_ignore_ascii_case("tool")
        || content.contains("[REDACTED_SECRET]") && content.trim().len() < 40
    {
        return Vec::new();
    }
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.len() < 12 || normalized.len() > 8_000 {
        return Vec::new();
    }
    let lower = normalized.to_lowercase();
    let (mem_type, confidence, tag) = if contains_any(
        &lower,
        &[
            "i prefer",
            "please always",
            "do not",
            "don't",
            "my preference",
            "ich bevorzuge",
            "bitte immer",
        ],
    ) {
        ("preference", 0.86, "captured-preference")
    } else if contains_any(
        &lower,
        &[
            "we decided",
            "decision:",
            "we will use",
            "chosen",
            "beschlossen",
            "entscheidung:",
        ],
    ) {
        ("decision", 0.84, "captured-decision")
    } else if contains_any(
        &lower,
        &[
            "remember that",
            "the project uses",
            "is required",
            "must be",
            "constraint:",
            "merke",
            "muss ",
        ],
    ) {
        ("fact", 0.76, "captured-fact")
    } else if contains_any(
        &lower,
        &[
            "the pattern is",
            "workflow",
            "always run",
            "procedure",
            "vorgehen",
            "ablauf",
        ],
    ) {
        ("pattern", 0.72, "captured-pattern")
    } else {
        return Vec::new();
    };
    if !policy
        .allowed_categories
        .iter()
        .any(|category| category == mem_type)
    {
        return Vec::new();
    }
    vec![ExtractedCandidate {
        content: bounded_chars(&normalized, 2_000),
        mem_type: mem_type.into(),
        tags: vec![tag.into(), "automatic-capture".into()],
        confidence,
    }]
}

fn redact_secrets(input: &str) -> (String, usize) {
    let mut count = 0;
    let mut words = Vec::new();
    let markers = [
        "api_key",
        "apikey",
        "token",
        "password",
        "passwd",
        "secret",
        "authorization",
        "bearer",
    ];
    let mut redact_next = false;
    for word in input.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        let looks_key = lower.starts_with("sk-")
            || lower.starts_with("ghp_")
            || lower.starts_with("github_pat_")
            || lower.starts_with("xoxb-")
            || (word.len() >= 32
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)));
        let marker_assignment = markers.iter().any(|marker| lower.contains(marker))
            && (lower.contains('=') || lower.contains(':'));
        if redact_next || looks_key || marker_assignment {
            words.push("[REDACTED_SECRET]");
            count += 1;
            redact_next = false;
            continue;
        }
        redact_next = markers
            .iter()
            .any(|marker| lower.trim_end_matches(':') == *marker);
        words.push(word);
    }
    (words.join(" "), count)
}

fn find_likely_contradiction(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    candidate: &ExtractedCandidate,
) -> BiResult<Option<String>> {
    if !matches!(
        candidate.mem_type.as_str(),
        "preference" | "decision" | "fact"
    ) {
        return Ok(None);
    }
    let terms: Vec<String> = candidate
        .content
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|term| term.len() >= 5)
        .take(4)
        .collect();
    if terms.is_empty() {
        return Ok(None);
    }
    let mut stmt = tx.prepare(
        "SELECT uid, content FROM memories WHERE project_id = ?1 AND mem_type = ?2
         AND superseded_by IS NULL ORDER BY updated_at DESC LIMIT 100",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id, candidate.mem_type], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let candidate_lower = candidate.content.to_lowercase();
    for row in rows {
        let (uid, existing) = row?;
        let existing_lower = existing.to_lowercase();
        let overlap = terms
            .iter()
            .filter(|term| existing_lower.contains(term.as_str()))
            .count();
        let polarity_changed = (candidate_lower.contains(" not ")
            || candidate_lower.contains("don't"))
            != (existing_lower.contains(" not ") || existing_lower.contains("don't"));
        if overlap >= 2 && polarity_changed {
            return Ok(Some(uid));
        }
    }
    Ok(None)
}

fn find_receipt(
    state: &AppState,
    project_id: &str,
    source_kind: &str,
    external_id: &str,
) -> BiResult<Option<(String, String)>> {
    let conn = state.db.conn()?;
    Ok(conn.query_row(
        "SELECT id, content_hash FROM observations WHERE project_id=?1 AND source_kind=?2 AND external_id=?3",
        rusqlite::params![project_id, source_kind, external_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?)
}

fn candidate_ids_for_observation(state: &AppState, observation_id: &str) -> BiResult<Vec<String>> {
    let conn = state.db.conn()?;
    let mut stmt = conn
        .prepare("SELECT id FROM memory_candidates WHERE observation_id=?1 ORDER BY created_at")?;
    let rows = stmt.query_map(rusqlite::params![observation_id], |row| row.get(0))?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn evidence_for(
    conn: &rusqlite::Connection,
    candidate_id: &str,
) -> BiResult<Vec<CandidateEvidence>> {
    let mut stmt = conn.prepare(
        "SELECT excerpt, source_pointer, source_timestamp, evidence_hash, extraction_method
         FROM candidate_evidence WHERE candidate_id=?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(rusqlite::params![candidate_id], |row| {
        Ok(CandidateEvidence {
            excerpt: row.get(0)?,
            source_pointer: row.get(1)?,
            source_timestamp: row.get(2)?,
            evidence_hash: row.get(3)?,
            extraction_method: row.get(4)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn row_to_candidate_base(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryCandidate> {
    let tags: String = row.get(5)?;
    Ok(MemoryCandidate {
        id: row.get(0)?,
        observation_id: row.get(1)?,
        project_id: row.get(2)?,
        content: row.get(3)?,
        mem_type: row.get(4)?,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        confidence: row.get(6)?,
        status: row.get(7)?,
        duplicate_memory_uid: row.get(8)?,
        contradiction_uid: row.get(9)?,
        resulting_memory_uid: row.get(10)?,
        version: row.get(11)?,
        evidence: Vec::new(),
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn row_to_capture_policy(row: &rusqlite::Row<'_>) -> rusqlite::Result<CapturePolicy> {
    let enabled_sources: String = row.get(1)?;
    let allowed_categories: String = row.get(2)?;
    let auto_categories: String = row.get(7)?;
    Ok(CapturePolicy {
        project_id: row.get(0)?,
        enabled_sources: serde_json::from_str(&enabled_sources).unwrap_or_default(),
        allowed_categories: serde_json::from_str(&allowed_categories).unwrap_or_default(),
        extraction_mode: row.get(3)?,
        ollama_endpoint: row.get(4)?,
        ollama_model: row.get(5)?,
        approval_mode: row.get(6)?,
        auto_approve_categories: serde_json::from_str(&auto_categories).unwrap_or_default(),
        evidence_max_chars: row.get::<_, i64>(8)? as usize,
        redaction_mode: row.get(9)?,
        notify_candidates: row.get::<_, i64>(10)? != 0,
        updated_at: row.get(11)?,
    })
}

fn validate_source_kind(value: &str) -> BiResult<()> {
    if matches!(value, "generic" | "codex" | "claude_code") {
        Ok(())
    } else {
        Err(BiError::Invalid(format!(
            "unsupported observation source {value}"
        )))
    }
}

fn is_loopback_endpoint(endpoint: &str) -> bool {
    endpoint.starts_with("http://127.0.0.1:")
        || endpoint.starts_with("http://localhost:")
        || endpoint.starts_with("http://[::1]:")
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn bounded_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        let dir =
            std::env::temp_dir().join(format!("biturbo-capture-test-{}", uuid::Uuid::new_v4()));
        AppState::open(&dir).unwrap()
    }

    #[test]
    fn observation_is_idempotent_and_never_stores_raw_content() {
        let state = state();
        let input = SubmitObservationInput {
            project_id: "default".into(),
            source_kind: "generic".into(),
            external_id: "evt-1".into(),
            session_id: None,
            occurred_at: 1,
            role: "user".into(),
            content: "I prefer concise answers and my api_key=sk-super-secret-value".into(),
            metadata: None,
        };
        let first = submit_observation(&state, input.clone()).unwrap();
        let second = submit_observation(&state, input).unwrap();
        assert!(!first.duplicate);
        assert!(second.duplicate);
        assert_eq!(first.observation_id, second.observation_id);
        let conn = state.db.conn().unwrap();
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('observations')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(!columns.iter().any(|column| column == "content"));
        let dump: String = conn
            .query_row(
                "SELECT content_hash FROM observations WHERE id=?1",
                rusqlite::params![first.observation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!dump.contains("super-secret"));
    }

    #[test]
    fn approving_candidate_creates_durable_memory_once() {
        let state = state();
        let result = submit_observation(
            &state,
            SubmitObservationInput {
                project_id: "default".into(),
                source_kind: "generic".into(),
                external_id: "evt-2".into(),
                session_id: None,
                occurred_at: 1,
                role: "user".into(),
                content: "We decided to use SQLite as the authoritative local store.".into(),
                metadata: None,
            },
        )
        .unwrap();
        let candidate_id = result.candidate_ids[0].clone();
        let decision = review_candidate(
            &state,
            CandidateDecisionInput {
                candidate_id: candidate_id.clone(),
                action: "approve".into(),
                edited_content: None,
                target_memory_uid: None,
                expected_version: 1,
                decided_by: Some("test".into()),
            },
        )
        .unwrap();
        assert_eq!(decision.candidate.status, "approved");
        assert!(
            crate::memory::get(&state, decision.memory_uid.as_deref().unwrap())
                .unwrap()
                .is_some()
        );
        let replay = review_candidate(
            &state,
            CandidateDecisionInput {
                candidate_id,
                action: "approve".into(),
                edited_content: None,
                target_memory_uid: None,
                expected_version: 1,
                decided_by: Some("test".into()),
            },
        )
        .unwrap();
        assert_eq!(replay.memory_uid, decision.memory_uid);
    }
}
