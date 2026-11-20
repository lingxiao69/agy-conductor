use chrono::{DateTime, Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsageRange {
    Today,
    #[default]
    Days7,
    Days30,
    AllTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileDailyModelBucket {
    pub date: String,
    pub model: String,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub steps: u64,
    #[serde(default)]
    pub session_ids: Vec<String>,
}

impl FileDailyModelBucket {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.output_tokens)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUsagePoint {
    pub date: String,
    pub label: String,
    pub total_cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider_cost: HashMap<String, f64>,
    pub by_provider_tokens: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsageRow {
    pub model_id: String,
    pub provider_id: String,
    pub steps: u64,
    pub sessions: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsageSummary {
    pub provider_id: String,
    pub display_name: String,
    pub sessions: u64,
    pub steps: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cache_hit_ratio: f64,
    pub cost_usd: f64,
    pub cost_share: f64,
    pub top_models: Vec<ModelUsageRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliUsageOverview {
    pub range: UsageRange,
    pub synced_at: String,
    pub total_cost_usd: f64,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub cache_hit_ratio: f64,
    pub total_sessions: u64,
    pub total_steps: u64,
    pub avg_daily_cost_usd: f64,
    pub today_cost_usd: f64,
    pub yesterday_cost_usd: f64,
    pub daily: Vec<DailyUsagePoint>,
    pub providers: Vec<ProviderUsageSummary>,
    pub models: Vec<ModelUsageRow>,
    pub scanned_files: usize,
    pub cache_hits: usize,
}

pub fn file_local_date(path: &Path) -> NaiveDate {
    let Ok(meta) = fs::metadata(path) else {
        return Local::now().date_naive();
    };
    let Ok(modified) = meta.modified() else {
        return Local::now().date_naive();
    };
    let Ok(dur) = modified.duration_since(UNIX_EPOCH) else {
        return Local::now().date_naive();
    };
    let dt: DateTime<Local> = Local
        .timestamp_opt(dur.as_secs() as i64, dur.subsec_nanos())
        .single()
        .unwrap_or_else(Local::now);
    dt.date_naive()
}

pub fn parse_local_date_str(
    ts_str: Option<&str>,
    epoch_ms: Option<i64>,
    fallback: NaiveDate,
) -> String {
    if let Some(raw) = ts_str.map(str::trim).filter(|s| !s.is_empty()) {
        if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
            return dt.with_timezone(&Local).format("%Y-%m-%d").to_string();
        }
        if raw.len() >= 10 {
            if let Ok(d) = NaiveDate::parse_from_str(&raw[..10], "%Y-%m-%d") {
                return d.format("%Y-%m-%d").to_string();
            }
        }
    }
    if let Some(ms) = epoch_ms.filter(|&v| v > 0) {
        let secs = if ms > 10_000_000_000 { ms / 1000 } else { ms };
        if let Some(dt) = Local.timestamp_opt(secs, 0).single() {
            return dt.format("%Y-%m-%d").to_string();
        }
    }
    fallback.format("%Y-%m-%d").to_string()
}

pub fn merge_bucket(
    map: &mut HashMap<(String, String), FileDailyModelBucket>,
    date: String,
    model: String,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    output_tokens: u64,
    session_id: &str,
) {
    let total = input_tokens
        .saturating_add(cache_read_tokens)
        .saturating_add(cache_creation_tokens)
        .saturating_add(output_tokens);
    if total == 0 {
        return;
    }
    let clean_model = if model.trim().is_empty() {
        "unknown".to_string()
    } else {
        model.trim().to_string()
    };
    let entry = map
        .entry((date.clone(), clean_model.clone()))
        .or_insert_with(|| FileDailyModelBucket {
            date,
            model: clean_model,
            ..FileDailyModelBucket::default()
        });
    entry.input_tokens = entry.input_tokens.saturating_add(input_tokens);
    entry.cache_read_tokens = entry.cache_read_tokens.saturating_add(cache_read_tokens);
    entry.cache_creation_tokens = entry
        .cache_creation_tokens
        .saturating_add(cache_creation_tokens);
    entry.output_tokens = entry.output_tokens.saturating_add(output_tokens);
    entry.steps = entry.steps.saturating_add(1);
    if !session_id.is_empty() && !entry.session_ids.iter().any(|s| s == session_id) {
        entry.session_ids.push(session_id.to_string());
    }
}
