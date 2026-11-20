use crate::billing::pricing::{resolve_official_rate, strip_model_prefix, TIER_THRESHOLD_TOKENS};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPriceRate {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_read_per_m: f64,
    pub cache_creation_per_m: f64,
}

#[derive(Debug, Clone, Default)]
pub struct PricingCatalog {
    overrides: HashMap<String, ModelPriceRate>,
}

impl PricingCatalog {
    pub fn builtin_only() -> Self {
        Self::default()
    }

    pub fn load_default() -> Self {
        match default_cc_switch_pricing_path() {
            Some(path) if path.is_file() => Self::from_file(&path),
            _ => Self::builtin_only(),
        }
    }

    pub fn from_file(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::builtin_only();
        };
        Self::from_json_str(&raw)
    }

    pub fn from_json_str(raw: &str) -> Self {
        let mut catalog = Self::default();
        let trimmed = raw.trim_start_matches('\u{FEFF}').trim();
        let Ok(root) = serde_json::from_str::<Value>(trimmed) else {
            return catalog;
        };
        let Some(models) = root.get("models").and_then(Value::as_array) else {
            return catalog;
        };

        for item in models {
            let Some(id) = item
                .get("modelId")
                .or_else(|| item.get("model_id"))
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(input_per_m) = parse_f64_field(item, &["inputCostPerMillion", "input_cost_per_million"]) else {
                continue;
            };
            let Some(output_per_m) = parse_f64_field(item, &["outputCostPerMillion", "output_cost_per_million"]) else {
                continue;
            };
            let cache_read_per_m = parse_f64_field(
                item,
                &["cacheReadCostPerMillion", "cache_read_cost_per_million"],
            )
            .unwrap_or(input_per_m * 0.1);
            let cache_creation_per_m = parse_f64_field(
                item,
                &[
                    "cacheCreationCostPerMillion",
                    "cache_creation_cost_per_million",
                ],
            )
            .unwrap_or_else(|| default_cache_creation_rate(id, input_per_m));

            let key = normalize_pricing_key(id);
            if !key.is_empty() {
                catalog.overrides.insert(
                    key,
                    ModelPriceRate {
                        input_per_m: input_per_m.max(0.0),
                        output_per_m: output_per_m.max(0.0),
                        cache_read_per_m: cache_read_per_m.max(0.0),
                        cache_creation_per_m: cache_creation_per_m.max(0.0),
                    },
                );
            }
        }
        catalog
    }

    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }

    pub fn resolve_rate(&self, model_id: &str, peak_context: u64) -> ModelPriceRate {
        if let Some(found) = self.lookup_override(model_id) {
            return found;
        }
        let tier = resolve_official_rate(model_id);
        let over_200k = peak_context > TIER_THRESHOLD_TOKENS;
        let (in_r, out_r, cache_r) = tier.rates_for_context(over_200k);
        ModelPriceRate {
            input_per_m: in_r,
            output_per_m: out_r,
            cache_read_per_m: cache_r,
            cache_creation_per_m: default_cache_creation_rate(model_id, in_r),
        }
    }

    pub fn calculate_cost_usd(
        &self,
        model_id: &str,
        input_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        output_and_thinking_tokens: u64,
        peak_context: u64,
    ) -> f64 {
        let rate = self.resolve_rate(model_id, peak_context);
        (input_tokens as f64 * rate.input_per_m
            + cache_read_tokens as f64 * rate.cache_read_per_m
            + cache_creation_tokens as f64 * rate.cache_creation_per_m
            + output_and_thinking_tokens as f64 * rate.output_per_m)
            / 1_000_000.0
    }

    fn lookup_override(&self, model_id: &str) -> Option<ModelPriceRate> {
        if self.overrides.is_empty() {
            return None;
        }
        let full = normalize_pricing_key(model_id);
        if full.is_empty() {
            return None;
        }
        if let Some(rate) = self.overrides.get(&full) {
            return Some(*rate);
        }

        let stripped_tier = strip_reasoning_suffix(&full);
        if let Some(rate) = self.overrides.get(stripped_tier) {
            return Some(*rate);
        }

        // 最长前缀匹配（例如 claude-opus-5-20260901 匹配 claude-opus-5）
        let mut best: Option<(usize, ModelPriceRate)> = None;
        for (key, rate) in &self.overrides {
            if key.len() < 5 {
                continue;
            }
            if stripped_tier.starts_with(key.as_str()) {
                let next_byte = stripped_tier.as_bytes().get(key.len());
                let boundary_ok = matches!(next_byte, None | Some(b'-') | Some(b':') | Some(b'@'));
                if boundary_ok && best.map(|(len, _)| key.len() > len).unwrap_or(true) {
                    best = Some((key.len(), *rate));
                }
            }
        }
        best.map(|(_, r)| r)
    }
}

pub fn default_cc_switch_pricing_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cc-switch").join("model-pricing.json"))
}

fn parse_f64_field(item: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(val) = item.get(*key) {
            if let Some(n) = val.as_f64() {
                if n.is_finite() {
                    return Some(n);
                }
            }
            if let Some(s) = val.as_str() {
                if let Ok(n) = s.trim().parse::<f64>() {
                    if n.is_finite() {
                        return Some(n);
                    }
                }
            }
        }
    }
    None
}

fn normalize_pricing_key(raw: &str) -> String {
    let base = strip_model_prefix(raw);
    let after_slash = base.rsplit('/').next().unwrap_or(base).trim();
    after_slash.to_ascii_lowercase()
}

fn strip_reasoning_suffix(lower: &str) -> &str {
    for suffix in ["-xhigh", "-high", "-medium", "-low", "-minimal", "-thinking"] {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            return stripped;
        }
    }
    lower
}

fn default_cache_creation_rate(model_id: &str, input_per_m: f64) -> f64 {
    if model_id.to_ascii_lowercase().contains("claude") {
        input_per_m * 1.25
    } else {
        input_per_m
    }
}
