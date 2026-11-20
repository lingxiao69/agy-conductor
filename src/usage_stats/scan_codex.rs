use crate::usage_stats::cache_store::UsageRollupCache;
use crate::usage_stats::types::{
    file_local_date, merge_bucket, parse_local_date_str, FileDailyModelBucket,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CodexTokenSnap {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

impl CodexTokenSnap {
    fn from_value(v: &Value) -> Self {
        Self {
            input_tokens: v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
            cached_input_tokens: v
                .get("cached_input_tokens")
                .or_else(|| v.get("cache_read_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_write_input_tokens: v
                .get("cache_write_input_tokens")
                .or_else(|| v.get("cache_creation_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
            total_tokens: v.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
        }
    }

    fn has_any(&self) -> bool {
        self.total_tokens > 0 || self.input_tokens > 0 || self.output_tokens > 0
    }

    fn delta_from(&self, prev: Option<Self>) -> Self {
        let Some(p) = prev else {
            return *self;
        };
        if self.total_tokens < p.total_tokens && self.input_tokens < p.input_tokens {
            return *self;
        }
        Self {
            input_tokens: self.input_tokens.saturating_sub(p.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(p.cached_input_tokens),
            cache_write_input_tokens: self
                .cache_write_input_tokens
                .saturating_sub(p.cache_write_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(p.output_tokens),
            total_tokens: self.total_tokens.saturating_sub(p.total_tokens),
        }
    }
}

pub fn scan_codex_usage(
    codex_home: &Path,
    cache: &mut UsageRollupCache,
    force_refresh: bool,
) -> (Vec<FileDailyModelBucket>, usize, usize) {
    let mut files = Vec::new();
    for sub in ["sessions", "archived_sessions"] {
        let dir = codex_home.join(sub);
        if dir.is_dir() {
            collect_jsonl_recursive(&dir, &mut files);
        }
    }
    if files.is_empty() && codex_home.is_dir() {
        collect_jsonl_recursive(codex_home, &mut files);
    }

    let mut all_buckets = Vec::new();
    let mut scanned = 0usize;
    let mut hits = 0usize;

    for path in files {
        scanned += 1;
        let (buckets, hit) = cache.lookup_or_parse("codex", &path, (0, 0), force_refresh, parse_codex_jsonl);
        if hit {
            hits += 1;
        }
        all_buckets.extend(buckets);
    }
    (all_buckets, scanned, hits)
}

fn parse_codex_jsonl(path: &Path) -> Vec<FileDailyModelBucket> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let fallback_date = file_local_date(path);
    let mut session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("codex-session")
        .to_string();
    let mut current_model = String::from("gpt-5-codex");
    let mut prev_total: Option<CodexTokenSnap> = None;
    let mut prev_last_sig: Option<CodexTokenSnap> = None;
    let mut buckets: HashMap<(String, String), FileDailyModelBucket> = HashMap::new();

    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(root) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let rec_type = root.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = root.get("payload");

        match rec_type {
            "session_meta" => {
                if let Some(p) = payload {
                    if let Some(id) = p.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                        session_id = id.to_string();
                    }
                    if let Some(m) = p.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                        current_model = m.to_string();
                    }
                }
            }
            "turn_context" => {
                if let Some(m) = payload
                    .and_then(|p| p.get("model"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    current_model = m.to_string();
                }
            }
            "event_msg" => {
                let Some(p) = payload else { continue };
                if p.get("type").and_then(Value::as_str) != Some("token_count") {
                    continue;
                }
                let Some(info) = p.get("info").filter(|v| v.is_object()) else {
                    continue;
                };
                if let Some(m) = info.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    current_model = m.to_string();
                }

                let total_snap = info
                    .get("total_token_usage")
                    .map(CodexTokenSnap::from_value)
                    .unwrap_or_default();
                let last_snap = info
                    .get("last_token_usage")
                    .map(CodexTokenSnap::from_value)
                    .unwrap_or_default();

                let delta = if total_snap.has_any() {
                    let d = total_snap.delta_from(prev_total);
                    prev_total = Some(total_snap);
                    d
                } else if last_snap.has_any() && prev_last_sig != Some(last_snap) {
                    prev_last_sig = Some(last_snap);
                    last_snap
                } else {
                    continue;
                };

                if !delta.has_any() {
                    continue;
                }

                // 关键语义修正：Codex 的 `input_tokens` 已包含 `cached_input_tokens`，
                // 且 `output_tokens` 已包含 `reasoning_output_tokens`，严禁重复叠加！
                let non_cached_input = delta.input_tokens.saturating_sub(delta.cached_input_tokens);
                let ts = root.get("timestamp").and_then(Value::as_str);
                let date = parse_local_date_str(ts, None, fallback_date);

                merge_bucket(
                    &mut buckets,
                    date,
                    current_model.clone(),
                    non_cached_input,
                    delta.cached_input_tokens,
                    delta.cache_write_input_tokens,
                    delta.output_tokens,
                    &session_id,
                );
            }
            _ => {}
        }
    }

    buckets.into_values().collect()
}

pub(crate) fn collect_jsonl_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_recursive(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("jsonl"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}
