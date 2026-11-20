use crate::usage_stats::cache_store::UsageRollupCache;
use crate::usage_stats::scan_codex::collect_jsonl_recursive;
use crate::usage_stats::types::{
    file_local_date, merge_bucket, parse_local_date_str, FileDailyModelBucket,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone)]
struct ClaudeMessageUsage {
    date: String,
    model: String,
    session_id: String,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    output_tokens: u64,
}

impl ClaudeMessageUsage {
    fn total(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.output_tokens)
    }
}

pub fn scan_claude_usage(
    claude_projects_dir: &Path,
    cache: &mut UsageRollupCache,
    force_refresh: bool,
) -> (Vec<FileDailyModelBucket>, usize, usize) {
    if !claude_projects_dir.is_dir() {
        return (Vec::new(), 0, 0);
    }
    let mut files = Vec::new();
    collect_jsonl_recursive(claude_projects_dir, &mut files);

    let mut all_buckets = Vec::new();
    let mut scanned = 0usize;
    let mut hits = 0usize;

    for path in files {
        scanned += 1;
        let (buckets, hit) =
            cache.lookup_or_parse("claude", &path, (0, 0), force_refresh, parse_claude_jsonl);
        if hit {
            hits += 1;
        }
        all_buckets.extend(buckets);
    }
    (all_buckets, scanned, hits)
}

fn parse_claude_jsonl(path: &Path) -> Vec<FileDailyModelBucket> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let fallback_date = file_local_date(path);
    let default_session = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("claude-session")
        .to_string();

    // 按 `message.id` 去重流式分片：同一消息保留最大 `output_tokens` 的最终条。
    let mut dedup_by_msg_id: HashMap<String, ClaudeMessageUsage> = HashMap::new();
    let reader = BufReader::new(file);

    for (line_idx, line) in reader.lines().map_while(Result::ok).enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(root) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(msg) = root.get("message").filter(|v| v.is_object()) else {
            continue;
        };
        let Some(usage) = msg.get("usage").filter(|v| v.is_object()) else {
            continue;
        };

        let input_tokens = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_read_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_creation_tokens = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        if input_tokens == 0
            && cache_read_tokens == 0
            && cache_creation_tokens == 0
            && output_tokens == 0
        {
            continue;
        }

        let msg_id = msg
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("line-{line_idx}"));
        let model = msg
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("claude-sonnet-5")
            .to_string();
        let session_id = root
            .get("sessionId")
            .or_else(|| root.get("session_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&default_session)
            .to_string();
        let ts = root.get("timestamp").and_then(Value::as_str);
        let date = parse_local_date_str(ts, None, fallback_date);

        let candidate = ClaudeMessageUsage {
            date,
            model,
            session_id,
            input_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            output_tokens,
        };

        match dedup_by_msg_id.get(&msg_id) {
            Some(existing)
                if existing.output_tokens > candidate.output_tokens
                    || (existing.output_tokens == candidate.output_tokens
                        && existing.total() >= candidate.total()) => {}
            _ => {
                dedup_by_msg_id.insert(msg_id, candidate);
            }
        }
    }

    let mut buckets: HashMap<(String, String), FileDailyModelBucket> = HashMap::new();
    for item in dedup_by_msg_id.into_values() {
        merge_bucket(
            &mut buckets,
            item.date,
            item.model,
            item.input_tokens,
            item.cache_read_tokens,
            item.cache_creation_tokens,
            item.output_tokens,
            &item.session_id,
        );
    }
    buckets.into_values().collect()
}
