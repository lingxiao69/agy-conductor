use crate::storage::store::atomic_write_text;
use crate::usage_stats::types::FileDailyModelBucket;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const CACHE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFileRollup {
    pub provider_id: String,
    pub mtime_ms: u64,
    pub file_len: u64,
    pub buckets: Vec<FileDailyModelBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRollupCache {
    version: u32,
    files: HashMap<String, CachedFileRollup>,
}

impl Default for PersistedRollupCache {
    fn default() -> Self {
        Self {
            version: CACHE_SCHEMA_VERSION,
            files: HashMap::new(),
        }
    }
}

pub struct UsageRollupCache {
    path: PathBuf,
    data: PersistedRollupCache,
    touched_keys: HashSet<String>,
    dirty: bool,
}

impl UsageRollupCache {
    pub fn load(path: PathBuf) -> Self {
        let data = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<PersistedRollupCache>(raw.trim()).ok())
            .filter(|c| c.version == CACHE_SCHEMA_VERSION)
            .unwrap_or_default();
        Self {
            path,
            data,
            touched_keys: HashSet::new(),
            dirty: false,
        }
    }

    pub fn lookup_or_parse<F>(
        &mut self,
        provider_id: &str,
        file_path: &Path,
        extra_stamp: (u64, u64),
        force_refresh: bool,
        parse_fn: F,
    ) -> (Vec<FileDailyModelBucket>, bool)
    where
        F: FnOnce(&Path) -> Vec<FileDailyModelBucket>,
    {
        let Some((base_mtime, base_len)) = file_fingerprint(file_path) else {
            return (Vec::new(), false);
        };
        let mtime_ms = base_mtime.max(extra_stamp.0);
        let file_len = base_len.saturating_add(extra_stamp.1);
        let key = cache_key(provider_id, file_path);
        self.touched_keys.insert(key.clone());

        if !force_refresh {
            if let Some(cached) = self.data.files.get(&key) {
                if cached.mtime_ms == mtime_ms && cached.file_len == file_len {
                    return (cached.buckets.clone(), true);
                }
            }
        }

        let buckets = parse_fn(file_path);
        self.data.files.insert(
            key,
            CachedFileRollup {
                provider_id: provider_id.to_string(),
                mtime_ms,
                file_len,
                buckets: buckets.clone(),
            },
        );
        self.dirty = true;
        (buckets, false)
    }

    pub fn save_if_dirty(&mut self) {
        let prev_len = self.data.files.len();
        self.data
            .files
            .retain(|k, _| self.touched_keys.contains(k));
        if self.data.files.len() != prev_len {
            self.dirty = true;
        }
        if !self.dirty {
            return;
        }
        if let Ok(json) = serde_json::to_string(&self.data) {
            let _ = atomic_write_text(&self.path, &json);
        }
        self.dirty = false;
    }
}

pub fn file_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some((mtime_ms, meta.len()))
}

fn cache_key(provider_id: &str, path: &Path) -> String {
    format!(
        "{}::{}",
        provider_id,
        path.to_string_lossy().replace('\\', "/").to_lowercase()
    )
}
