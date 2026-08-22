use crate::error::{BiError, BiResult};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tracing;
use turbovec::IdMapIndex;

/// All mutable index state behind one lock — `add`/`search`/`remove` each
/// need every map anyway, so a single acquisition beats four.
struct Inner {
    index: IdMapIndex,
    uid_to_extid: HashMap<String, u64>,
    extid_to_uid: HashMap<u64, String>,
    next_extid: u64,
}

pub struct ProjectIndex {
    pub project_id: String,
    pub dim: usize,
    pub bit_width: usize,
    inner: Mutex<Inner>,
    file_path: PathBuf,
    dirty: AtomicBool,
    last_change: Mutex<Instant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub uid: String,
    pub score: f32,
    pub ext_id: u64,
}

impl ProjectIndex {
    pub fn open_or_create(
        project_id: &str,
        dim: usize,
        bit_width: usize,
        data_dir: &Path,
    ) -> BiResult<Self> {
        if !matches!(bit_width, 2 | 3 | 4) {
            return Err(BiError::Invalid(format!(
                "bit_width must be 2, 3, or 4, got {bit_width}"
            )));
        }
        std::fs::create_dir_all(data_dir).ok();
        let file_path = data_dir.join(format!("{project_id}.tvim"));

        let (index, uid_to_extid, extid_to_uid, next_extid) = if file_path.exists() {
            match load_existing(project_id, &file_path, dim)? {
                Some(state) => state,
                None => {
                    // Sidecar/index pair was inconsistent and self-healed;
                    // start from a fresh empty index (AppState re-backfills
                    // from SQLite on next use).
                    let idx = IdMapIndex::new(dim, bit_width)
                        .map_err(|e| BiError::Index(format!("new: {e}")))?;
                    (idx, HashMap::new(), HashMap::new(), 1)
                }
            }
        } else {
            let idx =
                IdMapIndex::new(dim, bit_width).map_err(|e| BiError::Index(format!("new: {e}")))?;
            (idx, HashMap::new(), HashMap::new(), 1)
        };

        Ok(Self {
            project_id: project_id.to_string(),
            dim,
            bit_width,
            inner: Mutex::new(Inner {
                index,
                uid_to_extid,
                extid_to_uid,
                next_extid,
            }),
            file_path,
            dirty: AtomicBool::new(false),
            last_change: Mutex::new(Instant::now()),
        })
    }

    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    pub fn add(&self, uid: &str, vector: &[f32]) -> BiResult<()> {
        if vector.len() != self.dim {
            return Err(dim_mismatch_error(vector.len(), self.dim));
        }
        let mut inner = self.inner.lock();
        inner.add_one(uid, vector)?;
        drop(inner);
        self.mark_dirty();
        Ok(())
    }

    /// Add many (uid, vector) pairs under a single lock acquisition.
    /// New uids are appended in one `add_with_ids` call so turbovec can
    /// process them as a contiguous block.
    pub fn add_batch(&self, items: &[(String, Vec<f32>)]) -> BiResult<()> {
        if items.is_empty() {
            return Ok(());
        }
        // Validate every vector up front so a bad dim can't leave the maps
        // partially mutated.
        for (_, vector) in items {
            if vector.len() != self.dim {
                return Err(dim_mismatch_error(vector.len(), self.dim));
            }
        }
        let mut inner = self.inner.lock();
        let mut flat: Vec<f32> = Vec::with_capacity(items.len() * self.dim);
        let mut ids: Vec<u64> = Vec::with_capacity(items.len());
        let mut seen: HashSet<&str> = HashSet::new();
        for (uid, vector) in items {
            if !seen.insert(uid) {
                tracing::warn!(
                    "index: duplicate uid '{}' skipped in add_batch ({} items, ext-ids so far {})",
                    uid,
                    items.len(),
                    ids.len()
                );
                continue; // duplicate uid in same batch — skip
            }
            let extid = match inner.uid_to_extid.get(uid) {
                Some(&id) => {
                    let _ = inner.index.remove(id);
                    inner.extid_to_uid.remove(&id);
                    id
                }
                None => {
                    let id = inner.next_extid;
                    inner.next_extid += 1;
                    id
                }
            };
            inner.uid_to_extid.insert(uid.clone(), extid);
            inner.extid_to_uid.insert(extid, uid.clone());
            ids.push(extid);
            flat.extend_from_slice(vector);
        }
        inner
            .index
            .add_with_ids(&flat, &ids)
            .map_err(|e| BiError::Index(format!("add_batch: {e}")))?;
        drop(inner);
        self.mark_dirty();
        Ok(())
    }

    pub fn remove(&self, uid: &str) -> BiResult<bool> {
        let mut inner = self.inner.lock();
        if let Some(extid) = inner.uid_to_extid.remove(uid) {
            inner.extid_to_uid.remove(&extid);
            let removed = inner.index.remove(extid);
            drop(inner);
            self.mark_dirty();
            Ok(removed)
        } else {
            Ok(false)
        }
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
        *self.last_change.lock() = Instant::now();
    }

    /// Persist the index to disk if it's been dirty for at least `min_idle` and
    /// the last change was more than `min_idle` ago, or if `force` is true.
    /// Returns true if a write actually happened.
    pub fn maybe_flush(&self, min_idle: Duration, force: bool) -> BiResult<bool> {
        if !self.dirty.load(Ordering::Acquire) {
            return Ok(false);
        }
        if !force && self.last_change.lock().elapsed() < min_idle {
            return Ok(false);
        }
        self.persist_now()
    }

    /// Force a persist regardless of dirty/idle state.
    pub fn flush(&self) -> BiResult<bool> {
        self.persist_now()
    }

    fn persist_now(&self) -> BiResult<bool> {
        let inner = self.inner.lock();
        // Write to temp files then rename, so a crash mid-write never
        // corrupts the on-disk index.
        let tmp_index = self.file_path.with_extension("tvim.tmp");
        inner
            .index
            .write(&tmp_index)
            .map_err(|e| BiError::Index(format!("write: {e}")))?;
        let meta = meta_path_for(&self.file_path);
        let tmp_meta = meta.with_extension("json.tmp");
        let tvim_len = std::fs::metadata(&tmp_index)?.len();
        let envelope = UidMapEnvelope {
            version: 1,
            tvim_len,
            next_extid: inner.next_extid,
            map: inner.uid_to_extid.clone(),
        };
        {
            let file = std::fs::File::create(&tmp_meta)?;
            let mut w = std::io::BufWriter::new(file);
            serde_json::to_writer(&mut w, &envelope)?;
            w.flush()?;
        }
        // Durable persist: fsync both temp files before the renames, then
        // fsync the directory so the renames themselves survive a crash.
        std::fs::File::open(&tmp_index)?.sync_all()?;
        std::fs::File::open(&tmp_meta)?.sync_all()?;
        drop(inner);
        std::fs::rename(&tmp_index, &self.file_path)?;
        std::fs::rename(&tmp_meta, &meta)?;
        if let Some(dir) = self.file_path.parent() {
            // Directory fsync is unsupported on some platforms; ignore.
            if let Ok(dir_file) = std::fs::File::open(dir) {
                let _ = dir_file.sync_all();
            }
        }
        self.dirty.store(false, Ordering::Release);
        Ok(true)
    }

    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        allowlist_uids: Option<&[String]>,
    ) -> BiResult<Vec<SearchHit>> {
        if query.len() != self.dim {
            return Err(dim_mismatch_error(query.len(), self.dim));
        }
        let inner = self.inner.lock();

        let allowlist_extids: Option<Vec<u64>> = allowlist_uids.map(|uids| {
            uids.iter()
                .filter_map(|u| inner.uid_to_extid.get(u).copied())
                .collect()
        });

        let (scores, ids) = match allowlist_extids.as_ref() {
            // turbovec asserts non-empty allowlists; uids that map to no
            // extid mean nothing can match, so return no hits instead.
            Some(v) if !v.is_empty() => {
                inner
                    .index
                    .search_with_allowlist(query, k, Some(v.as_slice()))
            }
            Some(_) => return Ok(Vec::new()),
            None => inner.index.search(query, k),
        };

        Ok(ids
            .into_iter()
            .zip(scores)
            // turbovec pads short result rows with score -inf / ext id 0.
            .filter(|(_, score)| *score != f32::NEG_INFINITY)
            .filter_map(|(id, score)| {
                inner.extid_to_uid.get(&id).map(|uid| SearchHit {
                    uid: uid.clone(),
                    score,
                    ext_id: id,
                })
            })
            .collect())
    }

    /// Search the index, then return only hits whose uid is in `filter_uids`.
    /// Cheaper than `search_with_allowlist` when the candidate set is small
    /// relative to the index.
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        filter_uids: &[String],
    ) -> BiResult<Vec<SearchHit>> {
        if query.len() != self.dim {
            return Err(dim_mismatch_error(query.len(), self.dim));
        }
        let filter: HashSet<&str> = filter_uids.iter().map(|s| s.as_str()).collect();
        let inner = self.inner.lock();
        let (scores, ids) = inner.index.search(query, k);
        Ok(ids
            .into_iter()
            .zip(scores)
            // turbovec pads short result rows with score -inf / ext id 0.
            .filter(|(_, score)| *score != f32::NEG_INFINITY)
            .filter_map(|(id, score)| {
                inner
                    .extid_to_uid
                    .get(&id)
                    .filter(|uid| filter.contains(uid.as_str()))
                    .map(|uid| SearchHit {
                        uid: uid.clone(),
                        score,
                        ext_id: id,
                    })
            })
            .collect())
    }

    pub fn len(&self) -> usize {
        self.inner.lock().uid_to_extid.len()
    }

    pub fn contains_uid(&self, uid: &str) -> bool {
        self.inner.lock().uid_to_extid.contains_key(uid)
    }

    pub fn uid_digest(&self) -> String {
        let inner = self.inner.lock();
        let mut uids: Vec<&str> = inner.uid_to_extid.keys().map(String::as_str).collect();
        uids.sort_unstable();
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        for uid in uids {
            hasher.update(uid.as_bytes());
            hasher.update([0]);
        }
        hex::encode(hasher.finalize())
    }

    pub fn replace_all(&self, items: &[(String, Vec<f32>)]) -> BiResult<()> {
        {
            let mut inner = self.inner.lock();
            *inner = Inner {
                index: IdMapIndex::new(self.dim, self.bit_width)
                    .map_err(|e| BiError::Index(format!("new replacement index: {e}")))?,
                uid_to_extid: HashMap::new(),
                extid_to_uid: HashMap::new(),
                next_extid: 1,
            };
        }
        self.mark_dirty();
        self.add_batch(items)?;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Inner {
    fn add_one(&mut self, uid: &str, vector: &[f32]) -> BiResult<()> {
        let extid = match self.uid_to_extid.get(uid) {
            Some(&id) => {
                let _ = self.index.remove(id);
                self.extid_to_uid.remove(&id);
                id
            }
            None => {
                let id = self.next_extid;
                self.next_extid += 1;
                id
            }
        };
        self.index
            .add_with_ids(vector, &[extid])
            .map_err(|e| BiError::Index(format!("add: {e}")))?;
        self.uid_to_extid.insert(uid.to_string(), extid);
        self.extid_to_uid.insert(extid, uid.to_string());
        Ok(())
    }
}

fn meta_path_for(tvim: &Path) -> PathBuf {
    let mut p = tvim.to_path_buf();
    p.set_extension("uidmap.json");
    p
}

/// Versioned envelope for the `{pid}.uidmap.json` sidecar. `tvim_len` pins the
/// map to the exact .tvim it was written for, so a torn rename or mixed-up
/// file pair is detectable instead of silently mapping a live index onto
/// stale uids.
#[derive(Debug, Serialize, Deserialize)]
struct UidMapEnvelope {
    version: u32,
    tvim_len: u64,
    next_extid: u64,
    map: HashMap<String, u64>,
}

fn dim_mismatch_error(got: usize, expected: usize) -> BiError {
    BiError::Index(format!(
        "dimension mismatch: got {got}, project index expects {expected} — the embedding model likely changed; re-ingest or rebuild this project's index"
    ))
}

type LoadedState = (IdMapIndex, HashMap<String, u64>, HashMap<u64, String>, u64);

/// Load an existing .tvim + uidmap sidecar pair. Returns `Ok(None)` when the
/// pair was inconsistent and was self-healed (stale files deleted; the caller
/// must create a fresh empty index).
fn load_existing(project_id: &str, file_path: &Path, dim: usize) -> BiResult<Option<LoadedState>> {
    let idx = IdMapIndex::load(file_path)
        .map_err(|e| BiError::Index(format!("load {file_path:?}: {e}")))?;
    if idx.dim() != dim {
        return Err(BiError::Index(format!(
            "index dim {} != requested dim {dim} for project '{project_id}' — the embedding model likely changed; rebuild required",
            idx.dim()
        )));
    }
    let meta_path = meta_path_for(file_path);
    let tvim_len = std::fs::metadata(file_path)?.len();
    let mut map: HashMap<String, u64> = HashMap::new();
    let mut stored_next_extid: Option<u64> = None;
    let mut heal_reason: Option<String> = None;
    if meta_path.exists() {
        let bytes = std::fs::read(&meta_path)?;
        match serde_json::from_slice::<UidMapEnvelope>(&bytes) {
            Ok(env) if env.version == 1 => {
                if env.tvim_len != tvim_len {
                    heal_reason = Some(format!(
                        "uidmap tvim_len {} does not match .tvim length {tvim_len}",
                        env.tvim_len
                    ));
                } else {
                    map = env.map;
                    stored_next_extid = Some(env.next_extid);
                }
            }
            Ok(_) => {
                heal_reason =
                    Some("uidmap sidecar has an unsupported envelope version".to_string());
            }
            Err(_) => match serde_json::from_slice::<HashMap<String, u64>>(&bytes) {
                // Legacy pre-envelope sidecar: bare uid -> extid map, no
                // length pinning possible — accept as-is.
                // An empty legacy map over an existing .tvim is the exact
                // "empty id-map over live vectors" state we must never keep.
                Ok(bare) if bare.is_empty() => {
                    heal_reason = Some("legacy uidmap is empty while .tvim exists".to_string());
                }
                Ok(bare) => map = bare,
                Err(_) => {
                    heal_reason = Some("uidmap sidecar is corrupt/unparseable".to_string());
                }
            },
        }
    } else {
        heal_reason = Some("uidmap sidecar missing while .tvim exists".to_string());
    }
    if let Some(reason) = heal_reason {
        self_heal_stale_index(project_id, file_path, &reason)?;
        return Ok(None);
    }
    let extid_to_uid: HashMap<u64, String> =
        map.iter().map(|(uid, ext)| (*ext, uid.clone())).collect();
    let recomputed_next = map.values().copied().max().unwrap_or(0) + 1;
    let next_extid = match stored_next_extid {
        Some(stored) => stored.max(recomputed_next),
        None => recomputed_next,
    };
    Ok(Some((idx, map, extid_to_uid, next_extid)))
}

/// Delete a stale/inconsistent .tvim + sidecar pair (and any .tmp leftovers)
/// so a fresh empty index can be created. Never keep vectors without a
/// matching id map.
fn self_heal_stale_index(project_id: &str, file_path: &Path, reason: &str) -> BiResult<()> {
    tracing::warn!(
        "index: inconsistent on-disk pair for project '{project_id}' ({reason}); deleting stale index files and starting fresh"
    );
    let meta_path = meta_path_for(file_path);
    let _ = std::fs::remove_file(file_path);
    let _ = std::fs::remove_file(&meta_path);
    let _ = std::fs::remove_file(file_path.with_extension("tvim.tmp"));
    let _ = std::fs::remove_file(meta_path.with_extension("json.tmp"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_for(seed: f32, dim: usize) -> Vec<f32> {
        (0..dim).map(|i| (i as f32 * 0.01 + seed).sin()).collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("biturbo-test-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn add_batch_search_persist_roundtrip() {
        let dir = std::env::temp_dir().join(format!("biturbo-test-{}", uuid::Uuid::new_v4()));
        let dim = 32;
        let idx = ProjectIndex::open_or_create("t", dim, 4, &dir).unwrap();

        let items: Vec<(String, Vec<f32>)> = (0..50)
            .map(|i| (format!("uid-{i}"), vec_for(i as f32, dim)))
            .collect();
        idx.add_batch(&items).unwrap();
        idx.add("uid-extra", &vec_for(99.0, dim)).unwrap();
        assert_eq!(idx.len(), 51);

        // Re-adding an existing uid replaces, not duplicates.
        idx.add_batch(&[("uid-0".to_string(), vec_for(0.0, dim))])
            .unwrap();
        assert_eq!(idx.len(), 51);

        // Search returns hits; bit_width=4 quantization makes exact NN
        // ordering unreliable on synthetic vectors, so we only assert non-empty.
        let hits = idx.search(&vec_for(7.0, dim), 5, None).unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|h| h.uid.starts_with("uid-")));

        let filtered = idx
            .search_filtered(&vec_for(7.0, dim), 10, &["uid-7".to_string()])
            .unwrap();
        assert!(filtered.iter().all(|h| h.uid == "uid-7"));

        // Persist, reload, count preserved.
        assert!(idx.flush().unwrap());
        assert!(dir.join("t.tvim").exists());
        assert!(dir.join("t.uidmap.json").exists());
        let reloaded = ProjectIndex::open_or_create("t", dim, 4, &dir).unwrap();
        assert_eq!(reloaded.len(), 51);
        let hits = reloaded.search(&vec_for(7.0, dim), 5, None).unwrap();
        assert!(!hits.is_empty());

        assert!(reloaded.remove("uid-7").unwrap());
        assert_eq!(reloaded.len(), 50);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_dim_and_bit_width_return_err() {
        let dir = temp_dir("wrong-dim");
        let dim = 8;
        let idx = ProjectIndex::open_or_create("wd", dim, 4, &dir).unwrap();

        assert!(idx.add("a", &vec![0.0; 4]).is_err());
        assert!(idx.add_batch(&[("a".to_string(), vec![0.0; 4])]).is_err());
        assert!(idx.search(&vec![0.0; 4], 3, None).is_err());
        assert!(idx
            .search_filtered(&vec![0.0; 4], 3, &["a".to_string()])
            .is_err());
        // Nothing was added by the failed calls.
        assert_eq!(idx.len(), 0);

        // bit_width outside {2,3,4} is rejected before touching disk.
        assert!(ProjectIndex::open_or_create("wd-bw", dim, 5, &dir).is_err());

        // Reopening the same .tvim with a different dim must fail loudly.
        idx.add("a", &vec_for(1.0, dim)).unwrap();
        idx.flush().unwrap();
        assert!(ProjectIndex::open_or_create("wd", dim * 2, 4, &dir).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_uidmap_self_heals() {
        let dir = temp_dir("corrupt-uidmap");
        let dim = 8;
        let idx = ProjectIndex::open_or_create("sh", dim, 4, &dir).unwrap();
        idx.add("a", &vec_for(1.0, dim)).unwrap();
        idx.flush().unwrap();
        assert!(dir.join("sh.tvim").exists());

        std::fs::write(dir.join("sh.uidmap.json"), b"{corrupt json").unwrap();
        let reopened = ProjectIndex::open_or_create("sh", dim, 4, &dir).unwrap();
        assert_eq!(reopened.len(), 0);
        // The stale .tvim was deleted along with the corrupt meta.
        assert!(!dir.join("sh.tvim").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tvim_len_mismatch_self_heals() {
        let dir = temp_dir("tvim-len-mismatch");
        let dim = 8;
        let idx = ProjectIndex::open_or_create("sh", dim, 4, &dir).unwrap();
        idx.add("a", &vec_for(1.0, dim)).unwrap();
        idx.flush().unwrap();

        // Append a byte so the sidecar's tvim_len no longer matches.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("sh.tvim"))
            .unwrap();
        f.write_all(b"\x00").unwrap();

        let reopened = ProjectIndex::open_or_create("sh", dim, 4, &dir).unwrap();
        assert_eq!(reopened.len(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_bare_map_meta_still_loads() {
        let dir = temp_dir("legacy-meta");
        let dim = 8;
        let idx = ProjectIndex::open_or_create("lg", dim, 4, &dir).unwrap();
        idx.add("a", &vec_for(1.0, dim)).unwrap();
        idx.flush().unwrap();

        // Rewrite the sidecar as a legacy bare uid -> extid map (extid 1).
        let bare: HashMap<String, u64> = [("a".to_string(), 1u64)].into_iter().collect();
        std::fs::write(
            dir.join("lg.uidmap.json"),
            serde_json::to_vec(&bare).unwrap(),
        )
        .unwrap();

        let reloaded = ProjectIndex::open_or_create("lg", dim, 4, &dir).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded.contains_uid("a"));
        let hits = reloaded.search(&vec_for(1.0, dim), 1, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].uid, "a");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_never_returns_sentinel_pad_hits() {
        let dir = temp_dir("pads");
        let dim = 8;
        let idx = ProjectIndex::open_or_create("pd", dim, 4, &dir).unwrap();
        let items: Vec<(String, Vec<f32>)> = (0..3)
            .map(|i| (format!("uid-{i}"), vec_for(i as f32, dim)))
            .collect();
        idx.add_batch(&items).unwrap();

        let all_uids: Vec<String> = (0..3).map(|i| format!("uid-{i}")).collect();
        for hits in [
            idx.search(&vec_for(0.5, dim), 10, None).unwrap(),
            idx.search_filtered(&vec_for(0.5, dim), 10, &all_uids)
                .unwrap(),
        ] {
            // k=10 > 3 items: turbovec pads to k, but pads must be dropped.
            assert_eq!(hits.len(), 3);
            for h in hits {
                assert!(h.score != f32::NEG_INFINITY);
                assert!(h.ext_id != 0, "sentinel pad id leaked into results");
                assert!(h.uid.starts_with("uid-"));
            }
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn next_extid_survives_persist_reload() {
        let dir = temp_dir("next-extid");
        let dim = 8;
        let idx = ProjectIndex::open_or_create("ne", dim, 4, &dir).unwrap();
        idx.add("A", &vec_for(1.0, dim)).unwrap();
        idx.flush().unwrap();

        let idx = ProjectIndex::open_or_create("ne", dim, 4, &dir).unwrap();
        idx.add("B", &vec_for(2.0, dim)).unwrap();
        idx.flush().unwrap();

        let reloaded = ProjectIndex::open_or_create("ne", dim, 4, &dir).unwrap();
        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.contains_uid("A"));
        assert!(reloaded.contains_uid("B"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
