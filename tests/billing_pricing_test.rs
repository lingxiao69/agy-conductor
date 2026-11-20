use agy_conductor::agy::stream_parser::TokenUsage;
use agy_conductor::billing::{
    calculate_usage_cost_usd, resolve_official_rate, strip_model_prefix, PricingCatalog,
    SlotUsageStats,
};
use agy_conductor::orchestrator::engine_usage::SlotBillingLedger;

fn assert_close(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{label}: expected {expected:.9}, got {actual:.9}"
    );
}

#[test]
fn strips_gateway_prefix_and_resolves_all_official_model_tiers() {
    assert_eq!(
        strip_model_prefix("[Gateway] gemini-4-neon-xhigh"),
        "gemini-4-neon-xhigh"
    );
    assert_eq!(
        strip_model_prefix("  [gateway]   claude-sonnet-4-5 "),
        "claude-sonnet-4-5"
    );
    assert_eq!(strip_model_prefix("gemini-3.8-flash-low"), "gemini-3.8-flash-low");

    let neon = resolve_official_rate("[Gateway] gemini-4-neon-xhigh");
    assert_close(neon.input_le_200k, 3.00, "neon <=200k in");
    assert_close(neon.input_gt_200k, 6.00, "neon >200k in");
    assert_close(neon.output_le_200k, 15.00, "neon <=200k out");
    assert_close(neon.output_gt_200k, 22.50, "neon >200k out");
    assert_close(neon.cache_le_200k, 0.15, "neon <=200k cache");
    assert_close(neon.cache_gt_200k, 0.30, "neon >200k cache");

    let carbon = resolve_official_rate("[Gateway] gemini-4-carbon-high");
    assert_close(carbon.input_le_200k, 2.00, "carbon <=200k in");
    assert_close(carbon.input_gt_200k, 4.00, "carbon >200k in");
    assert_close(carbon.output_le_200k, 10.00, "carbon <=200k out");
    assert_close(carbon.output_gt_200k, 15.00, "carbon >200k out");
    assert_close(carbon.cache_le_200k, 0.10, "carbon <=200k cache");
    assert_close(carbon.cache_gt_200k, 0.20, "carbon >200k cache");

    let argon = resolve_official_rate("gemini-4-argon-medium");
    assert_eq!(argon, carbon);

    let hydro = resolve_official_rate("[Gateway] gemini-4-hydrogen-xhigh");
    assert_close(hydro.input_le_200k, 1.50, "hydrogen <=200k in");
    assert_close(hydro.output_le_200k, 7.50, "hydrogen <=200k out");
    assert_close(hydro.cache_le_200k, 0.075, "hydrogen <=200k cache");

    let flash = resolve_official_rate("gemini-3.8-flash-medium");
    assert_close(flash.input_le_200k, 0.75, "flash in");
    assert_close(flash.output_le_200k, 3.75, "flash out");
    assert_close(flash.cache_le_200k, 0.075, "flash cache");

    let flash_lite = resolve_official_rate("gemini-3.1-flash-lite");
    assert_close(flash_lite.input_le_200k, 0.30, "flash-lite in");
    assert_close(flash_lite.output_le_200k, 2.50, "flash-lite out");
    assert_close(flash_lite.cache_le_200k, 0.03, "flash-lite cache");

    let astra = resolve_official_rate("gpt-6.2-astra");
    assert_close(astra.input_le_200k, 8.00, "astra in");
    assert_close(astra.output_le_200k, 40.00, "astra out");
    assert_close(astra.cache_le_200k, 0.40, "astra cache");

    let sol = resolve_official_rate("gpt-6-sol");
    assert_close(sol.input_le_200k, 2.00, "sol in");
    assert_close(sol.output_le_200k, 10.00, "sol out");
    assert_close(sol.cache_le_200k, 0.20, "sol cache");

    let opus5 = resolve_official_rate("claude-opus-5");
    assert_close(opus5.input_le_200k, 5.00, "opus5 in");
    assert_close(opus5.output_le_200k, 25.00, "opus5 out");
    assert_close(opus5.cache_le_200k, 0.50, "opus5 cache");

    let sonnet5 = resolve_official_rate("claude-sonnet-5-20260801");
    assert_close(sonnet5.input_le_200k, 2.00, "sonnet5 in");
    assert_close(sonnet5.output_le_200k, 10.00, "sonnet5 out");
    assert_close(sonnet5.cache_le_200k, 0.20, "sonnet5 cache");

    let sonnet4 = resolve_official_rate("[Gateway] claude-sonnet-4-5");
    assert_close(sonnet4.input_le_200k, 3.00, "sonnet4 <=200k in");
    assert_close(sonnet4.input_gt_200k, 6.00, "sonnet4 >200k in");
    assert_close(sonnet4.output_gt_200k, 22.50, "sonnet4 >200k out");

    let codex = resolve_official_rate("gpt-5-codex");
    assert_close(codex.input_le_200k, 1.25, "codex in");
    assert_close(codex.output_le_200k, 10.00, "codex out");

    let fallback = resolve_official_rate("unknown-custom-model");
    assert_eq!(fallback, hydro);
}

#[test]
fn tiered_cost_bills_thinking_as_output_and_cache_at_discount_without_double_count() {
    let model = "[Gateway] gemini-4-neon-xhigh";

    // <= 200k context: input=100k ($0.30), cache=50k ($0.0075), output=10k + thinking=20k = 30k ($0.45) => $0.7575
    let u_le = TokenUsage {
        input_tokens: 100_000,
        cache_read_tokens: 50_000,
        output_tokens: 10_000,
        thinking_tokens: 20_000,
        total_tokens: 180_000,
    };
    let cost_le = calculate_usage_cost_usd(model, &u_le, 150_000);
    assert_close(cost_le, 0.30 + 0.0075 + 0.45, "neon <=200k cost");

    // > 200k context jump (peak = 250k): input=100k ($0.60), cache=150k ($0.045), out+think=30k ($0.675) => $1.32
    let u_gt = TokenUsage {
        input_tokens: 100_000,
        cache_read_tokens: 150_000,
        output_tokens: 10_000,
        thinking_tokens: 20_000,
        total_tokens: 280_000,
    };
    let cost_gt = calculate_usage_cost_usd(model, &u_gt, 250_000);
    assert_close(cost_gt, 0.60 + 0.045 + 0.675, "neon >200k cost");
}

#[test]
fn slot_billing_ledger_accumulates_in_flight_settle_and_interrupted_without_double_billing() {
    let model = "[Gateway] gemini-4-carbon-high";
    let mut ledger = SlotBillingLedger::default();

    // 轮次 1 进行中实时上报 (Carbon: in $2, cache $0.10, out $10)
    let live = ledger.apply_in_flight(
        model,
        TokenUsage {
            input_tokens: 50_000,
            cache_read_tokens: 10_000,
            output_tokens: 2_000,
            thinking_tokens: 3_000,
            total_tokens: 65_000,
        },
        60_000,
    );
    assert_eq!(live.total_tokens, 65_000);
    // (50k*2 + 10k*0.1 + 5k*10)/1M = 0.151
    assert_close(live.cost_usd, 0.151, "turn 1 live cost");

    // 轮次 1 结束收到权威 Result（逐项取最大值，绝不把 live + final 叠加双倍计费）
    let settled1 = ledger.settle_turn(
        model,
        TokenUsage {
            input_tokens: 50_000,
            cache_read_tokens: 10_000,
            output_tokens: 4_000,
            thinking_tokens: 6_000,
            total_tokens: 70_000,
        },
        60_000,
    );
    assert_eq!(settled1.total_tokens, 70_000);
    // (50k*2 + 10k*0.1 + 10k*10)/1M = 0.201
    assert_close(settled1.cost_usd, 0.201, "turn 1 settled cost");

    // 轮次 2 中途被用户取消，仍保留已产生的部分消耗与费用
    let _ = ledger.apply_in_flight(
        model,
        TokenUsage {
            input_tokens: 10_000,
            cache_read_tokens: 0,
            output_tokens: 1_000,
            thinking_tokens: 0,
            total_tokens: 11_000,
        },
        10_000,
    );
    let after_cancel = ledger
        .settle_interrupted(model, None, 10_000)
        .expect("interrupted turn with partial usage must settle");
    assert_eq!(after_cancel.total_tokens, 81_000);
    // + (10k*2 + 1k*10)/1M = +0.030 => 0.231
    assert_close(after_cancel.cost_usd, 0.231, "turn 2 interrupted cumulative cost");

    let empty_model_stats = SlotUsageStats::for_model(model);
    assert_eq!(empty_model_stats.rate_label, "G4 $2/$10/M");
    assert_eq!(empty_model_stats.total_tokens, 0);
}

#[test]
fn cc_switch_pricing_catalog_merges_overrides_and_cache_creation_cost() {
    let json = r#"{
        "models": [
            {
                "modelId": "claude-opus-5",
                "inputCostPerMillion": "4.5",
                "outputCostPerMillion": "22.0",
                "cacheReadCostPerMillion": "0.45",
                "cacheCreationCostPerMillion": "5.625"
            },
            {
                "modelId": "gemini-4-carbon",
                "inputCostPerMillion": 1.8,
                "outputCostPerMillion": 9.0,
                "cacheReadCostPerMillion": 0.09
            }
        ]
    }"#;
    let catalog = PricingCatalog::from_json_str(json);
    assert_eq!(catalog.override_count(), 2);

    // Date-suffixed claude model matches claude-opus-5 override
    let opus_cost = catalog.calculate_cost_usd(
        "claude-opus-5-20261101",
        100_000, // 0.45
        200_000, // 0.09
        100_000, // 0.5625
        50_000,  // 1.10
        0,
    );
    assert_close(opus_cost, 0.45 + 0.09 + 0.5625 + 1.10, "cc-switch opus override");

    // Gateway prefix & reasoning tier suffix normalized cleanly
    let carbon_rate = catalog.resolve_rate("[Gateway] gemini-4-carbon-high", 0);
    assert_close(carbon_rate.input_per_m, 1.8, "override carbon in");
    assert_close(carbon_rate.output_per_m, 9.0, "override carbon out");
    assert_close(carbon_rate.cache_read_per_m, 0.09, "override carbon cache");
}
