use crate::agy::runner::{AgentTurnError, AgentTurnResponse};
use crate::agy::stream_parser::AgentErrorKind;
use crate::orchestrator::schemas::{ArchitectOutput, CoderOutput, ReviewerOutput};
use crate::orchestrator::types::{
    AgentSlot, CompletionSummary, GroupChatMessage, MessageBadge, RunPhase,
    SlotContribution, SlotRunState, TaskItem,
};
use chrono::Local;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub enum TurnKind {
    ArchitectInitialPlan,
    ArchitectUserInterjection {
        notes: Vec<String>,
    },
    ArchitectIntervention {
        task_id: String,
        reason: String,
        round: u8,
    },
    ArchitectFinalGate,
    Coder {
        task_id: String,
        is_rework: bool,
        is_handoff: bool,
    },
    Reviewer {
        task_id: String,
    },
}

#[derive(Debug)]
pub struct FinishedTurn {
    pub turn_id: u64,
    pub slot_id: String,
    pub kind: TurnKind,
    pub result: Result<AgentTurnResponse, AgentTurnError>,
}

pub fn now_hhmmss() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

pub fn make_system_message(
    content: impl Into<String>,
    badge: MessageBadge,
    task_id: Option<String>,
) -> GroupChatMessage {
    GroupChatMessage {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: now_hhmmss(),
        sender_slot_id: None,
        sender_alias: "system".to_string(),
        sender_display: "轻舟协调器".to_string(),
        sender_role: None,
        priority: None,
        is_user: false,
        is_system: true,
        content: content.into(),
        badge,
        task_id,
        modified_files: Vec::new(),
        duration_seconds: None,
        tool_steps: None,
        issues: Vec::new(),
    }
}

pub fn make_user_message(content: impl Into<String>) -> GroupChatMessage {
    GroupChatMessage {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: now_hhmmss(),
        sender_slot_id: None,
        sender_alias: "user".to_string(),
        sender_display: "项目负责人 (你)".to_string(),
        sender_role: None,
        priority: None,
        is_user: true,
        is_system: false,
        content: content.into(),
        badge: MessageBadge::User,
        task_id: None,
        modified_files: Vec::new(),
        duration_seconds: None,
        tool_steps: None,
        issues: Vec::new(),
    }
}

pub fn make_slot_message(
    slot: &AgentSlot,
    content: String,
    badge: MessageBadge,
    task_id: Option<String>,
    modified_files: Vec<String>,
    resp: &AgentTurnResponse,
    issues: Vec<crate::orchestrator::types::ReviewIssue>,
) -> GroupChatMessage {
    GroupChatMessage {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: now_hhmmss(),
        sender_slot_id: Some(slot.slot_id.clone()),
        sender_alias: slot.alias.clone(),
        sender_display: slot.display_name.clone(),
        sender_role: Some(slot.role),
        priority: Some(slot.normalized_priority()),
        is_user: false,
        is_system: false,
        content,
        badge,
        task_id,
        modified_files,
        duration_seconds: Some(resp.duration_seconds),
        tool_steps: Some(resp.tool_steps),
        issues,
    }
}

pub fn parse_architect_output(resp: &AgentTurnResponse) -> Result<ArchitectOutput, String> {
    serde_json::from_value::<ArchitectOutput>(resp.structured_json.clone())
        .map_err(|e| format!("解析总架构师输出失败: {e}"))
}

pub fn parse_coder_output(resp: &AgentTurnResponse) -> Result<CoderOutput, String> {
    serde_json::from_value::<CoderOutput>(resp.structured_json.clone())
        .map_err(|e| format!("解析程序员输出失败: {e}"))
}

pub fn parse_reviewer_output(resp: &AgentTurnResponse) -> Result<ReviewerOutput, String> {
    serde_json::from_value::<ReviewerOutput>(resp.structured_json.clone())
        .map_err(|e| format!("解析审查员输出失败: {e}"))
}

/// Applies failure status to the slot and returns a user-friendly chat warning.
pub fn handle_turn_error_on_slot(
    slot: &mut AgentSlot,
    err: &AgentTurnError,
) -> (GroupChatMessage, bool) {
    if let Some(cid) = &err.conversation_id {
        slot.conversation_id = Some(cid.clone());
    }
    slot.current_activity = None;

    match err.kind {
        AgentErrorKind::QuotaExhausted => {
            slot.state = SlotRunState::QuotaCooling;
            slot.consecutive_failures += 1;
            let msg = make_system_message(
                format!(
                    "{} (@{}) 触发额度/限流限制，已切换为冷却状态，将自动调度同角色下一优先级模型接力。详情: {}",
                    slot.display_name, slot.alias, err.message
                ),
                MessageBadge::Warning,
                None,
            );
            (msg, true)
        }
        AgentErrorKind::Canceled => {
            if slot.state == SlotRunState::Working {
                slot.state = SlotRunState::Idle;
            }
            let msg = make_system_message(
                format!("{} (@{}) 本轮执行已取消。", slot.display_name, slot.alias),
                MessageBadge::Info,
                None,
            );
            (msg, false)
        }
        AgentErrorKind::Timeout | AgentErrorKind::InvalidOutput | AgentErrorKind::ProcessFailed => {
            slot.consecutive_failures += 1;
            slot.state = if slot.consecutive_failures >= 3 {
                SlotRunState::Error
            } else {
                SlotRunState::Idle
            };
            let msg = make_system_message(
                format!(
                    "{} (@{}) 执行异常（连续第 {} 次）：{}",
                    slot.display_name, slot.alias, slot.consecutive_failures, err.message
                ),
                MessageBadge::Warning,
                None,
            );
            (msg, false)
        }
    }
}

pub fn build_completion_summary(
    workspace: &str,
    goal: &str,
    elapsed_secs: u64,
    tasks: &[TaskItem],
    verified_commands: &[String],
    slots: &[AgentSlot],
    turn_counts: &HashMap<String, u32>,
    architect_note: String,
) -> CompletionSummary {
    let total_rework_rounds: u32 = tasks.iter().map(|t| t.rework_count as u32).sum();
    let mut seen_cmds = HashSet::new();
    let mut unique_cmds = Vec::new();
    for cmd in verified_commands {
        let c = cmd.trim();
        if !c.is_empty() && seen_cmds.insert(c.to_string()) {
            unique_cmds.push(c.to_string());
        }
    }

    let contributions: Vec<SlotContribution> = slots
        .iter()
        .filter_map(|s| {
            let turns = turn_counts.get(&s.slot_id).copied().unwrap_or(0);
            if turns > 0 || s.usage.total_tokens > 0 {
                Some(SlotContribution {
                    slot_id: s.slot_id.clone(),
                    display_name: s.display_name.clone(),
                    role: s.role,
                    turns,
                    usage: s.usage.clone(),
                })
            } else {
                None
            }
        })
        .collect();

    let total_tokens: u64 = slots
        .iter()
        .fold(0u64, |acc, s| acc.saturating_add(s.usage.total_tokens));
    let total_cost_usd: f64 = slots.iter().map(|s| s.usage.cost_usd).sum();

    CompletionSummary {
        workspace: workspace.to_string(),
        goal: goal.to_string(),
        total_duration_seconds: elapsed_secs,
        total_tasks: tasks.len(),
        total_rework_rounds,
        verified_commands: unique_cmds,
        contributions,
        architect_note,
        total_tokens,
        total_cost_usd,
    }
}

pub fn format_popup_summary(summary: &CompletionSummary) -> String {
    let mins = summary.total_duration_seconds / 60;
    let secs = summary.total_duration_seconds % 60;
    let duration_label = if mins > 0 {
        format!("{mins} 分 {secs} 秒")
    } else {
        format!("{secs} 秒")
    };
    let contrib_lines = summary
        .contributions
        .iter()
        .map(|c| {
            if c.usage.total_tokens > 0 {
                format!(
                    "· {} ({})：{} 轮（{} tok / ${:.4}）",
                    c.display_name,
                    c.role.label_zh(),
                    c.turns,
                    c.usage.total_tokens,
                    c.usage.cost_usd
                )
            } else {
                format!("· {} ({})：{} 轮", c.display_name, c.role.label_zh(), c.turns)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let cost_line = if summary.total_tokens > 0 {
        format!(
            "\n总消耗：{} tokens · 官方折算 ${:.4}",
            summary.total_tokens, summary.total_cost_usd
        )
    } else {
        String::new()
    };

    format!(
        "项目开发与审查已全部验收通过！\n\n项目目录：{}\n完成子任务：{} 项（累计返工 {} 轮）\n总耗时：{}{}\n\n模型贡献：\n{}\n\n总架构师验收结论：\n{}",
        summary.workspace,
        summary.total_tasks,
        summary.total_rework_rounds,
        duration_label,
        cost_line,
        if contrib_lines.is_empty() {
            "· 已完成".to_string()
        } else {
            contrib_lines
        },
        summary.architect_note
    )
}

pub fn phase_label_zh(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Idle => "待命",
        RunPhase::Planning => "架构规划中",
        RunPhase::Running => "编码与审查中",
        RunPhase::FinalGate => "全局终验中",
        RunPhase::Paused => "已暂停",
        RunPhase::WaitingUser => "等待人工拍板",
        RunPhase::Completed => "验收已完成",
        RunPhase::Stopped => "已停止",
        RunPhase::Failed => "运行异常",
    }
}
