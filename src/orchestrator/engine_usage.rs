use crate::agy::stream_parser::TokenUsage;
use crate::agy::usage_tracker::merge_elementwise_max;
use crate::billing::{calculate_usage_cost_usd, SlotUsageStats};
use crate::orchestrator::engine::{EngineEvent, OrchestratorEngine};

/// 单个角色槽位的账单台账：
/// - `settled_usage` + `settled_cost_usd` 记录已完成（或中断结算）轮次的累计消耗；
/// - `in_flight_usage` + `in_flight_peak_context` 记录当前正在执行轮次的实时增量；
/// - 展示与计费随时等于 `settled + in_flight`，正常收尾后将 `in_flight` 权威合并入 `settled` 并清零，绝不双倍计费。
#[derive(Debug, Clone, Default)]
pub struct SlotBillingLedger {
    pub settled_usage: TokenUsage,
    pub settled_cost_usd: f64,
    pub in_flight_usage: TokenUsage,
    pub in_flight_peak_context: u64,
    pub last_peak_context: u64,
}

impl SlotBillingLedger {
    pub fn apply_in_flight(
        &mut self,
        model_id: &str,
        usage: TokenUsage,
        peak_context: u64,
    ) -> SlotUsageStats {
        self.in_flight_usage = merge_elementwise_max(&self.in_flight_usage, &usage);
        self.in_flight_peak_context = self
            .in_flight_peak_context
            .max(peak_context)
            .max(
                self.in_flight_usage
                    .input_tokens
                    .saturating_add(self.in_flight_usage.cache_read_tokens),
            );
        self.to_stats(model_id)
    }

    pub fn settle_turn(
        &mut self,
        model_id: &str,
        final_usage: TokenUsage,
        peak_context: u64,
    ) -> SlotUsageStats {
        let settled_delta = merge_elementwise_max(&self.in_flight_usage, &final_usage);
        let turn_peak = self
            .in_flight_peak_context
            .max(peak_context)
            .max(
                settled_delta
                    .input_tokens
                    .saturating_add(settled_delta.cache_read_tokens),
            );

        let turn_cost = calculate_usage_cost_usd(model_id, &settled_delta, turn_peak);
        add_usage_inplace(&mut self.settled_usage, &settled_delta);
        self.settled_cost_usd += turn_cost;
        if turn_peak > 0 {
            self.last_peak_context = turn_peak;
        }
        self.in_flight_usage = TokenUsage::default();
        self.in_flight_peak_context = 0;

        self.to_stats(model_id)
    }

    pub fn settle_interrupted(
        &mut self,
        model_id: &str,
        partial_usage: Option<TokenUsage>,
        peak_context: u64,
    ) -> Option<SlotUsageStats> {
        let merged = match partial_usage {
            Some(p) => merge_elementwise_max(&self.in_flight_usage, &p),
            None => self.in_flight_usage.clone(),
        };
        if merged.total_tokens == 0 && self.in_flight_peak_context == 0 && peak_context == 0 {
            self.in_flight_usage = TokenUsage::default();
            self.in_flight_peak_context = 0;
            return None;
        }
        Some(self.settle_turn(model_id, merged, peak_context))
    }

    pub fn to_stats(&self, model_id: &str) -> SlotUsageStats {
        let mut combined = self.settled_usage.clone();
        add_usage_inplace(&mut combined, &self.in_flight_usage);

        let in_flight_cost = if self.in_flight_usage.total_tokens > 0 {
            calculate_usage_cost_usd(model_id, &self.in_flight_usage, self.in_flight_peak_context)
        } else {
            0.0
        };
        let display_peak = if self.in_flight_peak_context > 0 {
            self.in_flight_peak_context
        } else {
            self.last_peak_context
        };

        SlotUsageStats::from_totals(
            model_id,
            &combined,
            display_peak,
            self.settled_cost_usd + in_flight_cost,
        )
    }
}

fn add_usage_inplace(dst: &mut TokenUsage, delta: &TokenUsage) {
    dst.input_tokens = dst.input_tokens.saturating_add(delta.input_tokens);
    dst.output_tokens = dst.output_tokens.saturating_add(delta.output_tokens);
    dst.thinking_tokens = dst.thinking_tokens.saturating_add(delta.thinking_tokens);
    dst.cache_read_tokens = dst
        .cache_read_tokens
        .saturating_add(delta.cache_read_tokens);
    let sum = dst
        .input_tokens
        .saturating_add(dst.cache_read_tokens)
        .saturating_add(dst.output_tokens)
        .saturating_add(dst.thinking_tokens);
    dst.total_tokens = sum.max(dst.total_tokens.saturating_add(delta.total_tokens));
}

impl OrchestratorEngine {
    pub(super) fn update_slot_in_flight_usage(
        &mut self,
        slot_id: &str,
        usage: TokenUsage,
        peak_context: u64,
    ) {
        let Some(slot_idx) = self.slots.iter().position(|s| s.slot_id == slot_id) else {
            return;
        };
        let model_id = self.slots[slot_idx].model_id.clone();
        let stats = self
            .usage_ledgers
            .entry(slot_id.to_string())
            .or_default()
            .apply_in_flight(&model_id, usage, peak_context);

        if self.slots[slot_idx].usage != stats {
            self.slots[slot_idx].usage = stats.clone();
            let _ = self.event_tx.send(EngineEvent::SlotUsageUpdated {
                slot_id: slot_id.to_string(),
                usage: stats,
            });
        }
    }

    pub(super) fn settle_slot_turn_usage(
        &mut self,
        slot_id: &str,
        final_usage: TokenUsage,
        peak_context: u64,
    ) {
        let Some(slot_idx) = self.slots.iter().position(|s| s.slot_id == slot_id) else {
            return;
        };
        let model_id = self.slots[slot_idx].model_id.clone();
        let stats = self
            .usage_ledgers
            .entry(slot_id.to_string())
            .or_default()
            .settle_turn(&model_id, final_usage, peak_context);

        self.slots[slot_idx].usage = stats.clone();
        let _ = self.event_tx.send(EngineEvent::SlotUsageUpdated {
            slot_id: slot_id.to_string(),
            usage: stats,
        });
    }

    pub(super) fn settle_slot_interrupted_usage(
        &mut self,
        slot_id: &str,
        partial_usage: Option<TokenUsage>,
        peak_context: u64,
    ) {
        let Some(slot_idx) = self.slots.iter().position(|s| s.slot_id == slot_id) else {
            return;
        };
        let model_id = self.slots[slot_idx].model_id.clone();
        if let Some(stats) = self
            .usage_ledgers
            .entry(slot_id.to_string())
            .or_default()
            .settle_interrupted(&model_id, partial_usage, peak_context)
        {
            self.slots[slot_idx].usage = stats.clone();
            let _ = self.event_tx.send(EngineEvent::SlotUsageUpdated {
                slot_id: slot_id.to_string(),
                usage: stats,
            });
        }
    }

    pub(super) fn settle_all_in_flight_usages(&mut self) {
        let ids: Vec<String> = self.slots.iter().map(|s| s.slot_id.clone()).collect();
        for sid in ids {
            self.settle_slot_interrupted_usage(&sid, None, 0);
        }
    }

    pub(super) fn team_usage_totals(&self) -> (u64, f64, Vec<(String, SlotUsageStats)>) {
        let mut total_tokens: u64 = 0;
        let mut total_cost: f64 = 0.0;
        let mut per_slot = Vec::with_capacity(self.slots.len());
        for s in &self.slots {
            total_tokens = total_tokens.saturating_add(s.usage.total_tokens);
            total_cost += s.usage.cost_usd;
            per_slot.push((s.slot_id.clone(), s.usage.clone()));
        }
        (total_tokens, total_cost, per_slot)
    }
}
