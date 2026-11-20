use crate::billing::strip_model_prefix;
use crate::storage::brain_health::extract_step_token_quad;
use crate::storage::store::AppStorage;
use crate::storage::workspace_sessions::{load_all_workspace_role_sessions, read_room_role_sessions};
use crate::usage_stats::cache_store::UsageRollupCache;
use crate::usage_stats::types::{
    file_local_date, merge_bucket, parse_local_date_str, FileDailyModelBucket,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

const DEFAULT_AGY_MODEL_LABEL: &str = "gemini-4 (agy)";

pub fn scan_agy_usage(
    brain_dir: &Path,
    storage: Option<&AppStorage>,
    cache: &mut UsageRollupCache,
    force_refresh: bool,
) -> (Vec<FileDailyModelBucket>, usize, usize) {
    if !brain_dir.is_dir() {
        return (Vec::new(), 0, 0);
    }
    let conv_model_map = storage
        .map(build_conv_model_map)
        .unwrap_or_default();

    let Ok(entries) = fs::read_dir(brain_dir) else {
        return (Vec::new(), 0, 0);
    };

    let mut all_buckets = Vec::new();
    let mut scanned = 0usize;
    let mut hits = 0usize;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let conv_id = entry.file_name().to_string_lossy().to_string();
        let transcript = dir
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        if !transcript.is_file() {
            continue;
        }

        scanned += 1;
        let mapped_model = conv_model_map.get(&conv_id).cloned();
        let model_tag_hash = mapped_model
            .as_deref()
            .map(simple_str_hash)
            .unwrap_or(0)
            .saturating_add(2);
        let conv_clone = conv_id.clone();

        let (buckets, hit) = cache.lookup_or_parse(
            "agy",
            &transcript,
            (2, model_tag_hash),
            force_refresh,
            move |p| parse_agy_transcript(p, &conv_clone, mapped_model.as_deref()),
        );
        if hit {
            hits += 1;
        }
        all_buckets.extend(buckets);
    }

    (all_buckets, scanned, hits)
}

fn build_conv_model_map(storage: &AppStorage) -> HashMap<String, String> {
    let mut map = HashMap::new();

    for sess in load_all_workspace_role_sessions(storage) {
        insert_clean_model(&mut map, &sess.conversation_id, &sess.model_id);
    }

    if let Ok(rooms) = fs::read_dir(storage.rooms_dir()) {
        for room in rooms.flatten() {
            let rpath = room.path();
            if !rpath.is_dir() {
                continue;
            }
            for sess in read_room_role_sessions(&rpath) {
                insert_clean_model(&mut map, &sess.conversation_id, &sess.model_id);
            }
        }
    }

    let cfg = storage.load_config();
    for slot in cfg.slots {
        if let Some(cid) = slot.conversation_id {
            insert_clean_model(&mut map, &cid, &slot.model_id);
        }
    }

    map
}

fn insert_clean_model(map: &mut HashMap<String, String>, conv_id: &str, model_id: &str) {
    let cid = conv_id.trim();
    let clean = strip_model_prefix(model_id).trim();
    if !cid.is_empty() && !clean.is_empty() && !is_internal_model_placeholder(clean) {
        map.entry(cid.to_string())
            .or_insert_with(|| clean.to_string());
    }
}

fn parse_agy_transcript(
    path: &Path,
    conv_id: &str,
    mapped_model: Option<&str>,
) -> Vec<FileDailyModelBucket> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let fallback_date = file_local_date(path);
    let mut active_model = mapped_model
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_AGY_MODEL_LABEL)
        .to_string();
    let mut buckets: HashMap<(String, String), FileDailyModelBucket> = HashMap::new();

    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        if active_model == DEFAULT_AGY_MODEL_LABEL {
            if let Some(found) = infer_model_from_transcript_step(&val) {
                active_model = found;
            }
        }

        let Some((inp, cache_read, out, think)) = extract_step_token_quad(&val) else {
            continue;
        };
        let ts = val
            .get("created_at")
            .or_else(|| val.get("timestamp"))
            .and_then(Value::as_str);
        let date = parse_local_date_str(ts, None, fallback_date);
        let billable_out = out.saturating_add(think);

        merge_bucket(
            &mut buckets,
            date,
            active_model.clone(),
            inp,
            cache_read,
            0,
            billable_out,
            conv_id,
        );
    }

    buckets.into_values().collect()
}

fn is_internal_model_placeholder(name: &str) -> bool {
    name.trim()
        .as_bytes()
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"MODEL_"))
}

fn infer_model_from_transcript_step(val: &Value) -> Option<String> {
    for candidate in [
        val.get("model"),
        val.get("model_id"),
        val.get("metadata").and_then(|d| d.get("model")),
    ] {
        if let Some(m) = candidate.and_then(Value::as_str) {
            let clean = strip_model_prefix(m).trim();
            if !clean.is_empty() && !is_internal_model_placeholder(clean) {
                return Some(clean.to_string());
            }
        }
    }
    let step_type = val.get("type").and_then(Value::as_str).unwrap_or("");
    if step_type == "USER_INPUT" {
        if let Some(content) = val.get("content").and_then(Value::as_str) {
            return extract_model_mention_from_prompt(content);
        }
    }
    None
}

fn extract_model_mention_from_prompt(prompt: &str) -> Option<String> {
    for line in prompt.lines().take(36) {
        let trimmed = line.trim();
        for prefix in ["gemini-4-", "gemini-3.", "claude-opus-", "claude-sonnet-"] {
            if let Some(pos) = trimmed.to_ascii_lowercase().find(prefix) {
                let slice = &trimmed[pos..];
                let token: String = slice
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
                    .collect();
                if token.len() >= 10 {
                    return Some(token);
                }
            }
        }
    }
    None
}

fn simple_str_hash(s: &str) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h % 1_000_000_007
}
