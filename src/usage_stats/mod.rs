pub mod cache_store;
pub mod scan_agy;
pub mod scan_claude;
pub mod scan_codex;
pub mod scan_opencode;
pub mod types;
pub mod win_sqlite_ro;

pub use types::{
    CliUsageOverview, DailyUsagePoint, FileDailyModelBucket, ModelUsageRow, ProviderUsageSummary,
    UsageRange,
};

use crate::billing::PricingCatalog;
use crate::storage::brain_scanner::default_agy_brain_dir;
use crate::storage::store::AppStorage;
use crate::usage_stats::cache_store::UsageRollupCache;
use crate::usage_stats::scan_agy::scan_agy_usage;
use crate::usage_stats::scan_claude::scan_claude_usage;
use crate::usage_stats::scan_codex::scan_codex_usage;
use crate::usage_stats::scan_opencode::scan_opencode_usage;
use chrono::{Duration, Local, NaiveDate};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

const PROVIDERS_META: [(&str, &str); 4] = [
    ("agy", "Antigravity (agy)"),
    ("codex", "Codex CLI"),
    ("claude", "Claude Code"),
    ("opencode", "OpenCode"),
];

#[derive(Debug, Clone)]
pub struct UsageScanRoots {
    pub agy_brain_dir: Option<PathBuf>,
    pub codex_home_dir: Option<PathBuf>,
    pub claude_projects_dir: Option<PathBuf>,
    pub opencode_db_paths: Vec<PathBuf>,
    pub opencode_legacy_msg_dir: Option<PathBuf>,
    pub cache_path: PathBuf,
    pub storage_for_agy_models: Option<AppStorage>,
}

impl UsageScanRoots {
    pub fn default_for_storage(storage: &AppStorage) -> Self {
        let home = dirs::home_dir();
        let mut opencode_dbs = Vec::new();
        if let Some(ref h) = home {
            opencode_dbs.push(h.join(".local").join("share").join("opencode").join("opencode.db"));
        }
        if let Some(local_app) = dirs::data_local_dir() {
            opencode_dbs.push(local_app.join("opencode").join("opencode.db"));
        }
        Self {
            agy_brain_dir: default_agy_brain_dir(),
            codex_home_dir: home.as_ref().map(|h| h.join(".codex")),
            claude_projects_dir: home.as_ref().map(|h| h.join(".claude").join("projects")),
            opencode_db_paths: opencode_dbs,
            opencode_legacy_msg_dir: home
                .as_ref()
                .map(|h| h.join(".local").join("share").join("opencode").join("storage").join("message")),
            cache_path: storage.base_dir().join("cli_usage_rollups.json"),
            storage_for_agy_models: Some(storage.clone()),
        }
    }
}

pub fn collect_cli_usage_overview(
    storage: &AppStorage,
    range: UsageRange,
    force_refresh: bool,
) -> CliUsageOverview {
    let roots = UsageScanRoots::default_for_storage(storage);
    let pricing = PricingCatalog::load_default();
    collect_cli_usage_overview_with_roots(&roots, &pricing, range, force_refresh)
}

pub fn collect_cli_usage_overview_with_roots(
    roots: &UsageScanRoots,
    pricing: &PricingCatalog,
    range: UsageRange,
    force_refresh: bool,
) -> CliUsageOverview {
    let mut cache = UsageRollupCache::load(roots.cache_path.clone());
    let mut by_provider: HashMap<&'static str, Vec<FileDailyModelBucket>> = HashMap::new();
    let mut scanned_files = 0usize;
    let mut cache_hits = 0usize;

    if let Some(ref dir) = roots.agy_brain_dir {
        let (b, s, h) = scan_agy_usage(
            dir,
            roots.storage_for_agy_models.as_ref(),
            &mut cache,
            force_refresh,
        );
        scanned_files += s;
        cache_hits += h;
        by_provider.insert("agy", b);
    }
    if let Some(ref dir) = roots.codex_home_dir {
        let (b, s, h) = scan_codex_usage(dir, &mut cache, force_refresh);
        scanned_files += s;
        cache_hits += h;
        by_provider.insert("codex", b);
    }
    if let Some(ref dir) = roots.claude_projects_dir {
        let (b, s, h) = scan_claude_usage(dir, &mut cache, force_refresh);
        scanned_files += s;
        cache_hits += h;
        by_provider.insert("claude", b);
    }
    {
        let (b, s, h) = scan_opencode_usage(
            &roots.opencode_db_paths,
            roots.opencode_legacy_msg_dir.as_deref(),
            &mut cache,
            force_refresh,
        );
        scanned_files += s;
        cache_hits += h;
        by_provider.insert("opencode", b);
    }

    cache.save_if_dirty();

    aggregate_overview(
        by_provider,
        pricing,
        range,
        Local::now().date_naive(),
        scanned_files,
        cache_hits,
    )
}

#[derive(Default)]
struct AggBucket {
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    output_tokens: u64,
    steps: u64,
    cost_usd: f64,
    sessions: HashSet<String>,
}

impl AggBucket {
    fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.output_tokens)
    }

    fn cache_hit_ratio(&self) -> f64 {
        let denom = self
            .input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens);
        if denom == 0 {
            0.0
        } else {
            (self.cache_read_tokens as f64) / (denom as f64)
        }
    }

    fn absorb(&mut self, b: &FileDailyModelBucket, cost: f64, provider_prefix: &str) {
        self.input_tokens = self.input_tokens.saturating_add(b.input_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(b.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(b.cache_creation_tokens);
        self.output_tokens = self.output_tokens.saturating_add(b.output_tokens);
        self.steps = self.steps.saturating_add(b.steps);
        self.cost_usd += cost;
        for sid in &b.session_ids {
            self.sessions.insert(format!("{provider_prefix}:{sid}"));
        }
    }
}

fn aggregate_overview(
    by_provider: HashMap<&'static str, Vec<FileDailyModelBucket>>,
    pricing: &PricingCatalog,
    range: UsageRange,
    today: NaiveDate,
    scanned_files: usize,
    cache_hits: usize,
) -> CliUsageOverview {
    let today_str = today.format("%Y-%m-%d").to_string();
    let yesterday_str = (today - Duration::days(1)).format("%Y-%m-%d").to_string();
    let min_date_str = match range {
        UsageRange::Today => Some(today_str.clone()),
        UsageRange::Days7 => Some((today - Duration::days(6)).format("%Y-%m-%d").to_string()),
        UsageRange::Days30 => Some((today - Duration::days(29)).format("%Y-%m-%d").to_string()),
        UsageRange::AllTime => None,
    };

    let mut total_agg = AggBucket::default();
    let mut provider_aggs: HashMap<&'static str, AggBucket> = HashMap::new();
    let mut model_aggs: HashMap<(&'static str, String), AggBucket> = HashMap::new();
    let mut daily_map: BTreeMap<String, DailyUsagePoint> = BTreeMap::new();

    // 预填充固定日期坐标轴，确保柱状图间距均匀
    match range {
        UsageRange::Today => seed_daily_axis(&mut daily_map, today, 1),
        UsageRange::Days7 => seed_daily_axis(&mut daily_map, today, 7),
        UsageRange::Days30 => seed_daily_axis(&mut daily_map, today, 30),
        UsageRange::AllTime => {}
    }

    let mut today_cost_usd = 0.0f64;
    let mut yesterday_cost_usd = 0.0f64;

    for &(pid, _) in &PROVIDERS_META {
        let Some(buckets) = by_provider.get(pid) else {
            continue;
        };
        for b in buckets {
            let cost = pricing.calculate_cost_usd(
                &b.model,
                b.input_tokens,
                b.cache_read_tokens,
                b.cache_creation_tokens,
                b.output_tokens,
                0,
            );
            if b.date == today_str {
                today_cost_usd += cost;
            } else if b.date == yesterday_str {
                yesterday_cost_usd += cost;
            }

            if let Some(ref min_d) = min_date_str {
                if &b.date < min_d {
                    continue;
                }
            }

            total_agg.absorb(b, cost, pid);
            provider_aggs.entry(pid).or_default().absorb(b, cost, pid);
            model_aggs
                .entry((pid, b.model.clone()))
                .or_default()
                .absorb(b, cost, pid);

            let point = daily_map.entry(b.date.clone()).or_insert_with(|| DailyUsagePoint {
                label: short_date_label(&b.date),
                date: b.date.clone(),
                total_cost_usd: 0.0,
                total_tokens: 0,
                by_provider_cost: HashMap::new(),
                by_provider_tokens: HashMap::new(),
            });
            point.total_cost_usd += cost;
            point.total_tokens = point.total_tokens.saturating_add(b.total_tokens());
            *point.by_provider_cost.entry(pid.to_string()).or_insert(0.0) += cost;
            *point.by_provider_tokens.entry(pid.to_string()).or_insert(0) += b.total_tokens();
        }
    }

    let mut models: Vec<ModelUsageRow> = model_aggs
        .into_iter()
        .map(|((pid, model_id), agg)| ModelUsageRow {
            model_id,
            provider_id: pid.to_string(),
            steps: agg.steps,
            sessions: agg.sessions.len() as u64,
            input_tokens: agg.input_tokens,
            cache_read_tokens: agg.cache_read_tokens,
            cache_creation_tokens: agg.cache_creation_tokens,
            output_tokens: agg.output_tokens,
            total_tokens: agg.total_tokens(),
            cost_usd: agg.cost_usd,
        })
        .collect();
    models.sort_by(|a, b| {
        b.cost_usd
            .total_cmp(&a.cost_usd)
            .then_with(|| b.total_tokens.cmp(&a.total_tokens))
    });

    let grand_cost = total_agg.cost_usd;
    let providers: Vec<ProviderUsageSummary> = PROVIDERS_META
        .iter()
        .map(|&(pid, display_name)| {
            let agg = provider_aggs.get(pid);
            let cost_usd = agg.map(|a| a.cost_usd).unwrap_or(0.0);
            let cost_share = if grand_cost > 1e-12 {
                cost_usd / grand_cost
            } else {
                0.0
            };
            let top_models: Vec<ModelUsageRow> = models
                .iter()
                .filter(|m| m.provider_id == pid)
                .take(8)
                .cloned()
                .collect();
            ProviderUsageSummary {
                provider_id: pid.to_string(),
                display_name: display_name.to_string(),
                sessions: agg.map(|a| a.sessions.len() as u64).unwrap_or(0),
                steps: agg.map(|a| a.steps).unwrap_or(0),
                input_tokens: agg.map(|a| a.input_tokens).unwrap_or(0),
                cache_read_tokens: agg.map(|a| a.cache_read_tokens).unwrap_or(0),
                cache_creation_tokens: agg.map(|a| a.cache_creation_tokens).unwrap_or(0),
                output_tokens: agg.map(|a| a.output_tokens).unwrap_or(0),
                total_tokens: agg.map(AggBucket::total_tokens).unwrap_or(0),
                cache_hit_ratio: agg.map(AggBucket::cache_hit_ratio).unwrap_or(0.0),
                cost_usd,
                cost_share,
                top_models,
            }
        })
        .collect();

    let mut daily: Vec<DailyUsagePoint> = daily_map.into_values().collect();
    if range == UsageRange::AllTime && daily.len() > 30 {
        daily = daily.split_off(daily.len() - 30);
    }

    let active_days = daily.iter().filter(|d| d.total_tokens > 0).count().max(1);
    let avg_daily_cost_usd = total_agg.cost_usd / (active_days as f64);

    CliUsageOverview {
        range,
        synced_at: Local::now().format("%H:%M:%S").to_string(),
        total_cost_usd: total_agg.cost_usd,
        total_tokens: total_agg.total_tokens(),
        input_tokens: total_agg.input_tokens,
        cache_read_tokens: total_agg.cache_read_tokens,
        cache_creation_tokens: total_agg.cache_creation_tokens,
        output_tokens: total_agg.output_tokens,
        cache_hit_ratio: total_agg.cache_hit_ratio(),
        total_sessions: total_agg.sessions.len() as u64,
        total_steps: total_agg.steps,
        avg_daily_cost_usd,
        today_cost_usd,
        yesterday_cost_usd,
        daily,
        providers,
        models,
        scanned_files,
        cache_hits,
    }
}

fn seed_daily_axis(map: &mut BTreeMap<String, DailyUsagePoint>, today: NaiveDate, days: i64) {
    for offset in (0..days).rev() {
        let d = today - Duration::days(offset);
        let key = d.format("%Y-%m-%d").to_string();
        map.entry(key.clone()).or_insert_with(|| DailyUsagePoint {
            label: d.format("%m-%d").to_string(),
            date: key,
            total_cost_usd: 0.0,
            total_tokens: 0,
            by_provider_cost: HashMap::new(),
            by_provider_tokens: HashMap::new(),
        });
    }
}

fn short_date_label(date_str: &str) -> String {
    if date_str.len() >= 10 {
        date_str[5..10].to_string()
    } else {
        date_str.to_string()
    }
}
