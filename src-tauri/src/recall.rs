//! Explainable recall and local adaptive feedback.

use crate::error::{BiError, BiResult};
use crate::memory::{self, MemoryWithScore};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallExplanation {
    pub vector_rank: Option<usize>,
    pub fts_rank: Option<usize>,
    pub matched_terms: Vec<String>,
    pub feedback_boost: f32,
    pub applied_boosts: RankingBoosts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingBoosts {
    pub content_matches: usize,
    pub tag_matches: usize,
    pub path_matches: usize,
    pub language_match: bool,
    pub multiplier: f32,
    pub importance_boost: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainedMemory {
    #[serde(flatten)]
    pub hit: MemoryWithScore,
    pub explanation: RecallExplanation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallResponse {
    pub recall_id: String,
    pub results: Vec<ExplainedMemory>,
}

pub fn explain(
    state: &AppState,
    project_id: &str,
    query: &str,
    k: usize,
    mem_type: Option<&str>,
) -> BiResult<RecallResponse> {
    let project_id = if project_id.is_empty() {
        state.default_project_id.as_str()
    } else {
        project_id
    };
    let hits = memory::search(state, project_id, query, k, mem_type)?;
    let candidate_k = (k.clamp(1, 100) * 3).max(30);
    let allowlist: Option<Vec<String>> = if let Some(mem_type) = mem_type {
        let conn = state.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT uid FROM memories WHERE project_id = ?1 AND mem_type = ?2 AND superseded_by IS NULL",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id, mem_type], |r| r.get(0))?;
        Some(rows.filter_map(Result::ok).collect())
    } else {
        None
    };
    let vector_hits =
        state.embed_and_search(project_id, query, candidate_k, allowlist.as_deref())?;
    let conn = state.db.conn()?;
    let fts_hits = memory::fts_search(&conn, query, project_id, mem_type, candidate_k)?;
    drop(conn);
    let vector_ranks: HashMap<&str, usize> = vector_hits
        .iter()
        .enumerate()
        .map(|(rank, hit)| (hit.uid.as_str(), rank + 1))
        .collect();
    let fts_ranks: HashMap<&str, usize> = fts_hits
        .iter()
        .enumerate()
        .map(|(rank, (uid, _))| (uid.as_str(), rank + 1))
        .collect();
    let boosts = feedback_boosts(
        state,
        &hits
            .iter()
            .map(|hit| hit.memory.uid.clone())
            .collect::<Vec<_>>(),
    )?;
    let query_terms = normalized_terms(query);
    let results: Vec<ExplainedMemory> = hits
        .into_iter()
        .map(|hit| {
            let uid = hit.memory.uid.clone();
            let content = hit.memory.content.to_lowercase();
            let matched_terms = query_terms
                .iter()
                .filter(|term| content.contains(term.as_str()))
                .cloned()
                .collect();
            let applied_boosts = ranking_boosts(&hit.memory, &query_terms);
            ExplainedMemory {
                hit,
                explanation: RecallExplanation {
                    vector_rank: vector_ranks.get(uid.as_str()).copied(),
                    fts_rank: fts_ranks.get(uid.as_str()).copied(),
                    matched_terms,
                    feedback_boost: boosts.get(&uid).copied().unwrap_or(0.0),
                    applied_boosts,
                },
            }
        })
        .collect();
    let recall_id = format!("recall-{}", uuid::Uuid::new_v4());
    let result_uids: Vec<&str> = results
        .iter()
        .map(|item| item.hit.memory.uid.as_str())
        .collect();
    let explanations: Vec<&RecallExplanation> =
        results.iter().map(|item| &item.explanation).collect();
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO recall_events(id, project_id, query_hash, result_uids, explanations, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                recall_id,
                project_id,
                hash_query(query),
                serde_json::to_string(&result_uids)?,
                serde_json::to_string(&explanations)?,
                chrono::Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    })?;
    Ok(RecallResponse { recall_id, results })
}

pub(crate) fn fuse_rankings(
    vector_hits: &[crate::index_engine::SearchHit],
    fts_hits: &[(String, f64)],
) -> Vec<(String, f32)> {
    let mut fused = HashMap::new();
    const RRF_K: f32 = 60.0;
    for (rank, hit) in vector_hits.iter().enumerate() {
        let rank_score = 1.0 / (RRF_K + rank as f32 + 1.0);
        let similarity = hit.score.clamp(0.0, 1.0);
        *fused.entry(hit.uid.clone()).or_insert(0.0) += rank_score * (0.5 + 0.5 * similarity);
    }
    for (rank, (uid, _)) in fts_hits.iter().enumerate() {
        *fused.entry(uid.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
    }
    let mut ranked: Vec<(String, f32)> = fused.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
}

pub(crate) fn ranking_boosts(memory: &memory::Memory, terms: &[String]) -> RankingBoosts {
    let terms = memory::filter_stopwords(terms);
    if terms.is_empty() {
        return RankingBoosts {
            content_matches: 0,
            tag_matches: 0,
            path_matches: 0,
            language_match: false,
            multiplier: 1.0,
            importance_boost: 0.0,
        };
    }
    let content = memory.content.to_lowercase();
    let content_matches = terms
        .iter()
        .filter(|term| content.contains(term.as_str()))
        .count();
    let tag_matches = terms
        .iter()
        .filter(|term| {
            memory.tags.iter().any(|tag| {
                let tag = tag.to_lowercase();
                tag.contains(term.as_str()) || term.contains(&tag)
            })
        })
        .count();
    let path_matches = memory
        .file_path
        .as_ref()
        .map(|path| {
            let path = path.to_lowercase();
            terms
                .iter()
                .filter(|term| path.contains(term.as_str()))
                .count()
        })
        .unwrap_or(0);
    let language_match = memory.language.as_ref().is_some_and(|language| {
        let language = language.to_lowercase();
        terms.iter().any(|term| language.contains(term.as_str()))
    });
    let mut multiplier = 1.0;
    if content_matches > 0 {
        multiplier *= 1.0 + content_matches as f32 * 0.12;
    }
    if tag_matches > 0 {
        multiplier *= 1.0 + tag_matches as f32 * 0.20;
    }
    if path_matches > 0 {
        multiplier *= 1.0 + path_matches as f32 * 0.15;
    }
    if language_match {
        multiplier *= 1.08;
    }
    RankingBoosts {
        content_matches,
        tag_matches,
        path_matches,
        language_match,
        multiplier,
        importance_boost: memory.importance * 0.05,
    }
}

pub(crate) fn apply_ranking_boost(
    base_score: f32,
    memory: &memory::Memory,
    terms: &[String],
) -> f32 {
    let boosts = ranking_boosts(memory, terms);
    base_score * boosts.multiplier + boosts.importance_boost
}

pub fn submit_feedback(
    state: &AppState,
    recall_id: &str,
    memory_uid: &str,
    value: i8,
    source: &str,
) -> BiResult<()> {
    if !matches!(value, -1 | 1) {
        return Err(BiError::Invalid("feedback value must be -1 or 1".into()));
    }
    if !matches!(source, "explicit" | "implicit") {
        return Err(BiError::Invalid(
            "feedback source must be explicit or implicit".into(),
        ));
    }
    let conn = state.db.conn()?;
    let result_uids: String = conn
        .query_row(
            "SELECT result_uids FROM recall_events WHERE id = ?1",
            rusqlite::params![recall_id],
            |r| r.get(0),
        )
        .map_err(|_| BiError::NotFound(format!("recall {recall_id}")))?;
    let uids: Vec<String> = serde_json::from_str(&result_uids)?;
    if !uids.iter().any(|uid| uid == memory_uid) {
        return Err(BiError::Invalid(
            "memory was not part of this recall result".into(),
        ));
    }
    drop(conn);
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO recall_feedback(recall_id, memory_uid, value, source, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                recall_id,
                memory_uid,
                value,
                source,
                chrono::Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    })
}

pub fn feedback_boosts(state: &AppState, uids: &[String]) -> BiResult<HashMap<String, f32>> {
    if uids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat_n("?", uids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT memory_uid,
                COALESCE(SUM(CASE WHEN source = 'explicit' THEN value * 4 ELSE value END), 0)
         FROM recall_feedback WHERE memory_uid IN ({placeholders}) GROUP BY memory_uid"
    );
    let conn = state.db.conn()?;
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(uids), |r| {
        let uid: String = r.get(0)?;
        let weighted: f32 = r.get::<_, f64>(1)? as f32;
        Ok((uid, (weighted * 0.0025).clamp(-0.02, 0.02)))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn normalized_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_lowercase()
        })
        .filter(|term| term.len() >= 2)
        .collect()
}

fn hash_query(query: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(query.trim().as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{remember, RememberInput};

    #[test]
    fn explicit_feedback_produces_bounded_reversible_boost() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-recall-test-{}", uuid::Uuid::new_v4()));
        let state = AppState::open(&dir).unwrap();
        let memory = remember(
            &state,
            RememberInput {
                content: "calibrate the turbo wastegate actuator".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let response = explain(
            &state,
            &state.default_project_id,
            "turbo wastegate calibration",
            5,
            None,
        )
        .unwrap();
        submit_feedback(&state, &response.recall_id, &memory.uid, 1, "explicit").unwrap();
        let positive = feedback_boosts(&state, std::slice::from_ref(&memory.uid)).unwrap();
        assert!(positive[&memory.uid] > 0.0);
        assert!(positive[&memory.uid] <= 0.02);
        submit_feedback(&state, &response.recall_id, &memory.uid, -1, "explicit").unwrap();
        let neutral = feedback_boosts(&state, std::slice::from_ref(&memory.uid)).unwrap();
        assert_eq!(neutral[&memory.uid], 0.0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn fusion_breaks_equal_scores_by_uid() {
        let vector = vec![crate::index_engine::SearchHit {
            uid: "z-memory".into(),
            score: 1.0,
            ext_id: 1,
        }];
        let fts = vec![("a-memory".to_string(), 1.0)];
        let ranked = fuse_rankings(&vector, &fts);
        assert_eq!(ranked[0].0, "a-memory");
        assert_eq!(ranked[1].0, "z-memory");
    }
}
