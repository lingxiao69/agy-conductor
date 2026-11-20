pub mod cc_switch_pricing;
pub mod pricing;

pub use cc_switch_pricing::{default_cc_switch_pricing_path, ModelPriceRate, PricingCatalog};
pub use pricing::{
    calculate_usage_cost_usd, resolve_official_rate, strip_model_prefix, OfficialPriceTier,
    SlotUsageStats, DEFAULT_CONTEXT_LIMIT_TOKENS, TIER_THRESHOLD_TOKENS,
};
