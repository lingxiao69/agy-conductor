use crate::usage_stats::cache_store::{file_fingerprint, UsageRollupCache};
use crate::usage_stats::types::{
    file_local_date, merge_bucket, parse_local_date_str, FileDailyModelBucket,
};
use crate::usage_stats::win_sqlite_ro::query_opencode_messages_readonly;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub fn scan_opencode_usage(
    db_paths: &[PathBuf],
    legacy_msg_dir: Option<&Path>,
    cache: &mut UsageRollupCache,
    force_refresh: bool,
) -> (Vec<FileDailyModelBucket>, usize, usize) {
    let mut all_buckets = Vec::new();
    let mut scanned = 0usize;
    let mut hits = 0usize;
    let mut seen_db_paths = HashSet::new();

    for db_path in db_paths {
        if !db_path.is_file() {
            continue;
        }
        let norm = db_path.to_string_lossy().replace('\\', "/").to_lowercase();
        if !seen_db_paths.insert(norm) {
            continue;
        }
        scanned += 1;
        // 若存在 WAL 日志文件，将其指纹并入缓存键，确保新对话落盘即失效刷新。
        let wal_path = PathBuf::from(format!("{}-wal", db_path.to_string_lossy()));
        let wal_stamp = file_fingerprint(&wal_path).unwrap_or((0, 0));
        let (buckets, hit) = cache.lookup_or_parse(
            "opencode",
            db_path,
            wal_stamp,
            force_refresh,
            parse_opencode_sqlite_db,
        );
        if hit {
            hits += 1;
        }
        all_buckets.extend(buckets);
    }

    if let Some(dir) = legacy_msg_dir.filter(|d| d.is_dir()) {
        let mut json_files = Vec::new();
        collect_json_recursive(dir, &mut json_files);
        for path in json_files {
            scanned += 1;
            let (buckets, hit) = cache.lookup_or_parse(
                "opencode",
                &path,
                (0, 0),
                force_refresh,
                parse_opencode_legacy_json,
            );
            if hit {
                hits += 1;
            }
            all_buckets.extend(buckets);
        }
    }

    (all_buckets, scanned, hits)
}

fn parse_opencode_sqlite_db(db_path: &Path) -> Vec<FileDailyModelBucket> {
    let Ok(rows) = query_opencode_messages_readonly(db_path) else {
        return Vec::new();
    };
    let fallback_date = file_local_date(db_path);
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut buckets: HashMap<(String, String), FileDailyModelBucket> = HashMap::new();

    for row in rows {
        if !row.id.is_empty() && !seen_ids.insert(row.id.clone()) {
            continue;
        }
        let Ok(data) = serde_json::from_str::<Value>(&row.data_json) else {
            continue;
        };
        extract_opencode_message_into_buckets(
            &data,
            &row.session_id,
            Some(row.time_created),
            fallback_date,
            &mut buckets,
        );
    }
    buckets.into_values().collect()
}

fn parse_opencode_legacy_json(path: &Path) -> Vec<FileDailyModelBucket> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(data) = serde_json::from_str::<Value>(raw.trim()) else {
        return Vec::new();
    };
    let fallback_date = file_local_date(path);
    let session_id = data
        .get("sessionID")
        .or_else(|| data.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or("opencode-legacy")
        .to_string();
    let mut buckets: HashMap<(String, String), FileDailyModelBucket> = HashMap::new();
    extract_opencode_message_into_buckets(&data, &session_id, None, fallback_date, &mut buckets);
    buckets.into_values().collect()
}

fn extract_opencode_message_into_buckets(
    data: &Value,
    session_id: &str,
    row_time_created: Option<i64>,
    fallback_date: chrono::NaiveDate,
    buckets: &mut HashMap<(String, String), FileDailyModelBucket>,
) {
    let role = data.get("role").and_then(Value::as_str).unwrap_or("");
    if !role.eq_ignore_ascii_case("assistant") {
        return;
    }
    let Some(tokens) = data.get("tokens").filter(|v| v.is_object()) else {
        return;
    };

    let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
    let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
    let reasoning = tokens.get("reasoning").and_then(Value::as_u64).unwrap_or(0);
    let cache_obj = tokens.get("cache");
    let cache_read = cache_obj
        .and_then(|c| c.get("read"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = cache_obj
        .and_then(|c| c.get("write"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let billable_output = output.saturating_add(reasoning);
    if input == 0 && cache_read == 0 && cache_write == 0 && billable_output == 0 {
        return;
    }

    let model = data
        .get("modelID")
        .or_else(|| data.get("model_id"))
        .or_else(|| data.get("model"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("opencode-model")
        .to_string();

    let created_ms = data
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(Value::as_i64)
        .or(row_time_created);
    let date = parse_local_date_str(None, created_ms, fallback_date);

    merge_bucket(
        buckets,
        date,
        model,
        input,
        cache_read,
        cache_write,
        billable_output,
        session_id,
    );
}

fn collect_json_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_recursive(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}
