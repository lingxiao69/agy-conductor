use crate::agy::stream_parser::TokenUsage;
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONTEXT_LIMIT_TOKENS: u64 = 200_000;
pub const TIER_THRESHOLD_TOKENS: u64 = 200_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OfficialPriceTier {
    pub tier_name: &'static str,
    pub short_label: &'static str,
    pub input_le_200k: f64,
    pub input_gt_200k: f64,
    pub output_le_200k: f64,
    pub output_gt_200k: f64,
    pub cache_le_200k: f64,
    pub cache_gt_200k: f64,
}

impl OfficialPriceTier {
    pub fn rates_for_context(&self, over_200k: bool) -> (f64, f64, f64) {
        if over_200k {
            (self.input_gt_200k, self.output_gt_200k, self.cache_gt_200k)
        } else {
            (self.input_le_200k, self.output_le_200k, self.cache_le_200k)
        }
    }

    pub fn format_detail(&self, over_200k: bool) -> String {
        let (in_r, out_r, cache_r) = self.rates_for_context(over_200k);
        let tier_hint = if over_200k && (self.input_gt_200k > self.input_le_200k) {
            " (>200k 阶梯价)"
        } else {
            ""
        };
        format!(
            "{}{tier_hint} (输入 ${}/M · 输出+思考 ${}/M · 缓存 ${}/M)",
            self.tier_name,
            fmt_rate(in_r),
            fmt_rate(out_r),
            fmt_rate(cache_r)
        )
    }
}

fn fmt_rate(v: f64) -> String {
    if (v * 100.0 - (v * 100.0).round()).abs() > 1e-6 {
        format!("{v:.3}")
    } else {
        format!("{v:.2}")
    }
}

const TIER_GEMINI_4_NEON: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Gemini 4 Neon 官方价",
    short_label: "Neon $3/$15/M",
    input_le_200k: 3.00,
    input_gt_200k: 6.00,
    output_le_200k: 15.00,
    output_gt_200k: 22.50,
    cache_le_200k: 0.15,
    cache_gt_200k: 0.30,
};

const TIER_GEMINI_4_CARBON_ARGON: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Gemini 4 Carbon/Argon 官方价",
    short_label: "G4 $2/$10/M",
    input_le_200k: 2.00,
    input_gt_200k: 4.00,
    output_le_200k: 10.00,
    output_gt_200k: 15.00,
    cache_le_200k: 0.10,
    cache_gt_200k: 0.20,
};

const TIER_GEMINI_4_HYDROGEN: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Gemini 4 Hydrogen 官方价",
    short_label: "Hydro $1.5/$7.5/M",
    input_le_200k: 1.50,
    input_gt_200k: 3.00,
    output_le_200k: 7.50,
    output_gt_200k: 11.25,
    cache_le_200k: 0.075,
    cache_gt_200k: 0.15,
};

const TIER_GEMINI_3_PRO: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Gemini 3 Pro 官方旗舰价",
    short_label: "Pro $2/$12/M",
    input_le_200k: 2.00,
    input_gt_200k: 4.00,
    output_le_200k: 12.00,
    output_gt_200k: 18.00,
    cache_le_200k: 0.20,
    cache_gt_200k: 0.40,
};

const TIER_GEMINI_25_PRO: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Gemini 2.5 Pro 官方标准价",
    short_label: "Pro $1.25/$10/M",
    input_le_200k: 1.25,
    input_gt_200k: 2.50,
    output_le_200k: 10.00,
    output_gt_200k: 15.00,
    cache_le_200k: 0.125,
    cache_gt_200k: 0.25,
};

const TIER_GEMINI_3_FLASH: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Gemini 3 Flash 官方标准价",
    short_label: "Flash $0.75/$3.75/M",
    input_le_200k: 0.75,
    input_gt_200k: 0.75,
    output_le_200k: 3.75,
    output_gt_200k: 3.75,
    cache_le_200k: 0.075,
    cache_gt_200k: 0.075,
};

const TIER_GEMINI_FLASH_LITE: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Gemini Flash-Lite 官方价",
    short_label: "Lite $0.30/$2.50/M",
    input_le_200k: 0.30,
    input_gt_200k: 0.30,
    output_le_200k: 2.50,
    output_gt_200k: 2.50,
    cache_le_200k: 0.03,
    cache_gt_200k: 0.03,
};

const TIER_CLAUDE_OPUS: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Claude Opus 官方标准价",
    short_label: "Opus $5/$25/M",
    input_le_200k: 5.00,
    input_gt_200k: 5.00,
    output_le_200k: 25.00,
    output_gt_200k: 25.00,
    cache_le_200k: 0.50,
    cache_gt_200k: 0.50,
};

const TIER_CLAUDE_SONNET_5: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Claude Sonnet 5 官方标准价",
    short_label: "Sonnet5 $2/$10/M",
    input_le_200k: 2.00,
    input_gt_200k: 2.00,
    output_le_200k: 10.00,
    output_gt_200k: 10.00,
    cache_le_200k: 0.20,
    cache_gt_200k: 0.20,
};

const TIER_CLAUDE_SONNET_4: OfficialPriceTier = OfficialPriceTier {
    tier_name: "Claude Sonnet 4 官方标准价",
    short_label: "Sonnet $3/$15/M",
    input_le_200k: 3.00,
    input_gt_200k: 6.00,
    output_le_200k: 15.00,
    output_gt_200k: 22.50,
    cache_le_200k: 0.30,
    cache_gt_200k: 0.60,
};

const TIER_GPT_62_ASTRA: OfficialPriceTier = OfficialPriceTier {
    tier_name: "GPT-6.2 Astra 官方旗舰价",
    short_label: "Astra $8/$40/M",
    input_le_200k: 8.00,
    input_gt_200k: 8.00,
    output_le_200k: 40.00,
    output_gt_200k: 40.00,
    cache_le_200k: 0.40,
    cache_gt_200k: 0.40,
};

const TIER_GPT_6_SOL: OfficialPriceTier = OfficialPriceTier {
    tier_name: "GPT-6 Sol 官方标准价",
    short_label: "Sol $2/$10/M",
    input_le_200k: 2.00,
    input_gt_200k: 2.00,
    output_le_200k: 10.00,
    output_gt_200k: 10.00,
    cache_le_200k: 0.20,
    cache_gt_200k: 0.20,
};

const TIER_OPENAI_CODEX: OfficialPriceTier = OfficialPriceTier {
    tier_name: "OpenAI Codex 官方标准价",
    short_label: "Codex $1.25/$10/M",
    input_le_200k: 1.25,
    input_gt_200k: 1.25,
    output_le_200k: 10.00,
    output_gt_200k: 10.00,
    cache_le_200k: 0.125,
    cache_gt_200k: 0.125,
};

/// 剥离 `[Gateway]` 等代理前缀并规范化模型标识。
pub fn strip_model_prefix(model_id: &str) -> &str {
    let trimmed = model_id.trim();
    if trimmed.len() >= 9 && trimmed[..9].eq_ignore_ascii_case("[gateway]") {
        trimmed[9..].trim()
    } else {
        trimmed
    }
}

/// 按官方公开 API 单价解析模型对应的计费档位（大小写不敏感，对齐实机模型价目表）。
pub fn resolve_official_rate(model_id: &str) -> OfficialPriceTier {
    let stripped = strip_model_prefix(model_id);
    let tail = stripped.rsplit('/').next().unwrap_or(stripped);
    let lower = tail.to_ascii_lowercase();

    if lower.starts_with("gemini-4-neon") {
        return TIER_GEMINI_4_NEON;
    }
    if lower.starts_with("gemini-4-carbon") || lower.starts_with("gemini-4-argon") {
        return TIER_GEMINI_4_CARBON_ARGON;
    }
    if lower.starts_with("gemini-4-hydrogen") || lower.starts_with("gemini-4 (agy)") {
        return TIER_GEMINI_4_HYDROGEN;
    }
    if lower.starts_with("gemini-3.1-pro") || lower.starts_with("gemini-3-pro") {
        return TIER_GEMINI_3_PRO;
    }
    if lower.starts_with("gemini-2.5-pro") {
        return TIER_GEMINI_25_PRO;
    }
    if lower.starts_with("gemini-") && lower.contains("flash-lite") {
        return TIER_GEMINI_FLASH_LITE;
    }
    if lower.starts_with("gemini-3") && lower.contains("flash") {
        return TIER_GEMINI_3_FLASH;
    }
    if lower.starts_with("gpt-6.2-astra") {
        return TIER_GPT_62_ASTRA;
    }
    if lower.starts_with("gpt-6") && lower.contains("sol") {
        return TIER_GPT_6_SOL;
    }
    if lower.starts_with("claude-opus-5") || lower.starts_with("claude-opus-4") {
        return TIER_CLAUDE_OPUS;
    }
    if lower.starts_with("claude-sonnet-5") {
        return TIER_CLAUDE_SONNET_5;
    }
    if lower.starts_with("claude-sonnet-4") {
        return TIER_CLAUDE_SONNET_4;
    }
    if lower.starts_with("gpt-5") || lower.starts_with("codex") || lower.starts_with("o3") {
        return TIER_OPENAI_CODEX;
    }

    TIER_GEMINI_4_HYDROGEN
}

/// 精准按官方阶梯价计算单步/单轮费用（美元）：
/// - `input_tokens` 按输入单价；
/// - `cache_read_tokens` 按缓存读取折扣价（绝不重复按输入全价计费）；
/// - `output_tokens + thinking_tokens` 按输出单价。
pub fn calculate_usage_cost_usd(model_id: &str, usage: &TokenUsage, peak_context: u64) -> f64 {
    let rate = resolve_official_rate(model_id);
    let prompt_context = peak_context.max(usage.input_tokens.saturating_add(usage.cache_read_tokens));
    let over_200k = prompt_context > TIER_THRESHOLD_TOKENS;
    let (in_r, out_r, cache_r) = rate.rates_for_context(over_200k);
    let billable_out = usage.output_tokens.saturating_add(usage.thinking_tokens);
    (usage.input_tokens as f64 * in_r
        + usage.cache_read_tokens as f64 * cache_r
        + billable_out as f64 * out_r)
        / 1_000_000.0
}

fn default_context_limit() -> u64 {
    DEFAULT_CONTEXT_LIMIT_TOKENS
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SlotUsageStats {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub thinking_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub peak_context_tokens: u64,
    #[serde(default = "default_context_limit")]
    pub context_limit_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub rate_label: String,
    #[serde(default)]
    pub rate_detail: String,
}

impl Default for SlotUsageStats {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            thinking_tokens: 0,
            cache_read_tokens: 0,
            total_tokens: 0,
            peak_context_tokens: 0,
            context_limit_tokens: DEFAULT_CONTEXT_LIMIT_TOKENS,
            cost_usd: 0.0,
            rate_label: String::new(),
            rate_detail: String::new(),
        }
    }
}

impl SlotUsageStats {
    pub fn for_model(model_id: &str) -> Self {
        let rate = resolve_official_rate(model_id);
        Self {
            rate_label: rate.short_label.to_string(),
            rate_detail: rate.format_detail(false),
            ..Self::default()
        }
    }

    pub fn from_totals(
        model_id: &str,
        usage: &TokenUsage,
        peak_context_tokens: u64,
        cost_usd: f64,
    ) -> Self {
        let rate = resolve_official_rate(model_id);
        let over_200k = peak_context_tokens > TIER_THRESHOLD_TOKENS;
        let computed_total = usage
            .input_tokens
            .saturating_add(usage.cache_read_tokens)
            .saturating_add(usage.output_tokens)
            .saturating_add(usage.thinking_tokens);
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            thinking_tokens: usage.thinking_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            total_tokens: computed_total.max(usage.total_tokens),
            peak_context_tokens,
            context_limit_tokens: DEFAULT_CONTEXT_LIMIT_TOKENS,
            cost_usd,
            rate_label: rate.short_label.to_string(),
            rate_detail: rate.format_detail(over_200k),
        }
    }
}
