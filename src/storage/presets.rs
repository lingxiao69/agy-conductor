use crate::agy::gateway_wildcards::sanitize_wildcard_model_id;
use crate::billing::SlotUsageStats;
use crate::orchestrator::types::{AgentSlot, SameRoleDispatchMode, SlotRunState};
use crate::storage::store::PersistedConfig;
use chrono::Local;
use serde::{Deserialize, Serialize};

const MAX_PRESET_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RosterPreset {
    pub preset_id: String,
    pub name: String,
    pub dispatch_mode: SameRoleDispatchMode,
    pub slots: Vec<AgentSlot>,
    pub updated_at: String,
}

/// 清洗单个槽位的运行态字段并归一化优先级（1..=99），避免将旧会话 ID 或忙碌态写入预设。
pub fn sanitize_slot_for_preset(mut slot: AgentSlot) -> AgentSlot {
    slot.model_id = sanitize_wildcard_model_id(&slot.model_id);
    slot.priority = slot.normalized_priority();
    slot.state = SlotRunState::Idle;
    slot.conversation_id = None;
    slot.session_reused = false;
    slot.current_activity = None;
    slot.consecutive_failures = 0;
    slot.usage = SlotUsageStats::for_model(&slot.model_id);
    slot
}

/// 批量清洗槽位列表用于编队预设保存或非运行态切换。
pub fn sanitize_slots_for_preset(slots: Vec<AgentSlot>) -> Vec<AgentSlot> {
    slots.into_iter().map(sanitize_slot_for_preset).collect()
}

/// 新增或按名称覆盖保存编队预设，并同步更新 `active_preset_id`。
/// 返回 `(保存后的预设, 是否为同名覆盖更新)`。
pub fn save_preset(
    config: &mut PersistedConfig,
    name: &str,
    slots: Vec<AgentSlot>,
    dispatch_mode: SameRoleDispatchMode,
) -> Result<(RosterPreset, bool), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("预设名称不能为空".to_string());
    }
    if trimmed.chars().count() > MAX_PRESET_NAME_CHARS {
        return Err(format!("预设名称不能超过 {MAX_PRESET_NAME_CHARS} 个字符"));
    }
    if slots.is_empty() {
        return Err("编队预设至少需要包含 1 个模型槽位".to_string());
    }

    let sanitized_slots = sanitize_slots_for_preset(slots);
    let now = Local::now().to_rfc3339();

    if let Some(existing) = config
        .presets
        .iter_mut()
        .find(|p| p.name.trim().eq_ignore_ascii_case(trimmed))
    {
        existing.name = trimmed.to_string();
        existing.dispatch_mode = dispatch_mode;
        existing.slots = sanitized_slots;
        existing.updated_at = now;
        let saved = existing.clone();
        config.active_preset_id = Some(saved.preset_id.clone());
        return Ok((saved, true));
    }

    let preset = RosterPreset {
        preset_id: format!("preset-{}", uuid::Uuid::new_v4().simple()),
        name: trimmed.to_string(),
        dispatch_mode,
        slots: sanitized_slots,
        updated_at: now,
    };
    config.active_preset_id = Some(preset.preset_id.clone());
    config.presets.push(preset.clone());
    Ok((preset, false))
}

/// 按 ID 查找预设。
pub fn find_preset<'a>(config: &'a PersistedConfig, preset_id: &str) -> Option<&'a RosterPreset> {
    let target = preset_id.trim();
    if target.is_empty() {
        return None;
    }
    config.presets.iter().find(|p| p.preset_id == target)
}

/// 在存储配置层直接应用指定预设（重置运行态与旧会话 ID，归一化优先级并设置 `active_preset_id`）。
pub fn apply_preset_to_config(
    config: &mut PersistedConfig,
    preset_id: &str,
) -> Result<RosterPreset, String> {
    let preset = find_preset(config, preset_id)
        .cloned()
        .ok_or_else(|| format!("未找到指定的编队预设: {preset_id}"))?;

    config.slots = sanitize_slots_for_preset(preset.slots.clone());
    config.dispatch_mode = preset.dispatch_mode;
    config.active_preset_id = Some(preset.preset_id.clone());
    Ok(preset)
}

/// 按 ID 删除预设；若删除的是当前激活预设，则自动将 `active_preset_id` 置为 `None`。
pub fn delete_preset(config: &mut PersistedConfig, preset_id: &str) -> Result<RosterPreset, String> {
    let target = preset_id.trim();
    let Some(idx) = config.presets.iter().position(|p| p.preset_id == target) else {
        return Err(format!("未找到待删除的编队预设: {preset_id}"));
    };
    let removed = config.presets.remove(idx);
    if config.active_preset_id.as_deref() == Some(removed.preset_id.as_str()) {
        config.active_preset_id = None;
    }
    Ok(removed)
}
