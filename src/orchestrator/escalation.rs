use crate::orchestrator::types::{
    AgentSlot, RoleKind, SlotRunState, TaskItem, TaskStatus, UserBlockerAlert,
};

/// 同一子任务在升级给用户拍板之前，总架构师最多自动静默裁决的次数。
pub const MAX_ARCH_ARBITRATION_ROUNDS: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationDecision {
    None,
    ArbitrateByArchitect {
        task_id: String,
        reason: String,
        round: u8,
    },
    EscalateToUser {
        task_id: Option<String>,
        alert: UserBlockerAlert,
    },
}

/// 二级阻塞升级评估：
/// - 第 1 级（静默优先裁决）：同一子任务阻塞/超返工且 `arch_arbitration_count < 2`，交由总架构师优先内部拍板；
/// - 第 2 级（人工拍板弹窗）：总架构师已自动裁决 2 次后第 3 次仍陷入阻塞/超返工，转为 `NeedsUserDecision` 并生成通俗大白话提醒。
pub fn evaluate_blocked_task_escalation(
    tasks: &mut [TaskItem],
    max_rework_rounds: u8,
) -> EscalationDecision {
    for task in tasks.iter_mut() {
        if task.status == TaskStatus::BlockedByArch {
            let raw_reason = task
                .blocker_reason
                .clone()
                .unwrap_or_else(|| "子任务在模块接口或改动范围上存在冲突".to_string());
            if task.arch_arbitration_count < MAX_ARCH_ARBITRATION_ROUNDS {
                task.arch_arbitration_count += 1;
                let round = task.arch_arbitration_count;
                return EscalationDecision::ArbitrateByArchitect {
                    task_id: task.task_id.clone(),
                    reason: raw_reason,
                    round,
                };
            }

            task.status = TaskStatus::NeedsUserDecision;
            let plain_reason = simplify_reason_for_human(&raw_reason);
            let alert = UserBlockerAlert {
                title: "技术方案多次碰撞，需要您拍板".to_string(),
                plain_explanation: format!(
                    "【卡住的功能】「{}」（{}）\n\
                     【大白话原因】这个功能连续碰到了实现上的冲突，总架构师已经先帮您自动调整过 {} 次方案，但仍然无法单方面敲定（核心卡点：{}）。\n\
                     【请您选择推进方式】\n\
                     回复 1：允许放宽改动范围，让总架构师把关联模块一起统一调整好；\n\
                     回复 2：先保持其他模块不动，按最保守的兼容方案分步实现；\n\
                     或者直接在下方输入您的想法，系统会立刻交给总架构师继续执行。",
                    task.title, task.task_id, MAX_ARCH_ARBITRATION_ROUNDS, plain_reason
                ),
                task_id: Some(task.task_id.clone()),
                reason_kind: "arbitration_limit_reached".to_string(),
            };
            return EscalationDecision::EscalateToUser {
                task_id: Some(task.task_id.clone()),
                alert,
            };
        }

        if task.status == TaskStatus::ReworkNeeded && task.rework_count >= max_rework_rounds {
            if task.arch_arbitration_count < MAX_ARCH_ARBITRATION_ROUNDS {
                task.arch_arbitration_count += 1;
                let round = task.arch_arbitration_count;
                return EscalationDecision::ArbitrateByArchitect {
                    task_id: task.task_id.clone(),
                    reason: format!(
                        "子任务 {} ({}) 已连续打回修改 {} 轮（达到上限 {} 轮），请优先从架构层面重新拆解、明确边界或换用更稳妥的方案继续推进。",
                        task.task_id, task.title, task.rework_count, max_rework_rounds
                    ),
                    round,
                };
            }

            task.status = TaskStatus::NeedsUserDecision;
            let review_hint = task
                .last_review_note
                .as_deref()
                .map(simplify_reason_for_human)
                .unwrap_or_else(|| "反复修改后仍有验收项未完全达标".to_string());
            let alert = UserBlockerAlert {
                title: "功能反复修改仍未过审，需要您拍板".to_string(),
                plain_explanation: format!(
                    "【卡住的功能】「{}」（{}）\n\
                     【大白话原因】程序员和审查员来回修改检查了很多轮，总架构师也自动协调过 {} 次，目前仍卡在验收细节上（{}）。\n\
                     【请您选择推进方式】\n\
                     回复 1：请总架构师把这个功能拆成更简单的两步来重新做；\n\
                     回复 2：放宽这项细节要求，优先保证核心主流程先跑通交付；\n\
                     或者直接告诉我您希望怎么改。",
                    task.title, task.task_id, MAX_ARCH_ARBITRATION_ROUNDS, review_hint
                ),
                task_id: Some(task.task_id.clone()),
                reason_kind: "rework_limit_reached".to_string(),
            };
            return EscalationDecision::EscalateToUser {
                task_id: Some(task.task_id.clone()),
                alert,
            };
        }
    }
    EscalationDecision::None
}

/// 情况 A：总架构师经过审慎评估后主动返回 `NEED_USER_CLARIFICATION`。
pub fn build_clarification_alert(
    chat_message: &str,
    user_plain_explanation: Option<&str>,
    task_id: Option<String>,
) -> UserBlockerAlert {
    let preferred = user_plain_explanation
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| chat_message.trim());
    let plain_explanation = if preferred.is_empty() {
        "【需要您确认】总架构师评估后发现当前目标有多种做法，拿不定主意该按哪种业务习惯来实现。\n\
         请查看群聊记录并直接回复您的决定，团队会立刻继续。"
            .to_string()
    } else {
        preferred.to_string()
    };

    UserBlockerAlert {
        title: "总架构师需要您拍板确认".to_string(),
        plain_explanation,
        task_id,
        reason_kind: "need_user_clarification".to_string(),
    }
}

/// 情况 C：所有可用槽位停摆（配额耗尽/错误/暂停/缺失角色）或存在遗留的待人工确认任务。
pub fn build_all_slots_unavailable_alert(
    slots: &[AgentSlot],
    tasks: &[TaskItem],
) -> UserBlockerAlert {
    if let Some(stuck) = tasks
        .iter()
        .find(|t| t.status == TaskStatus::NeedsUserDecision)
    {
        let reason = stuck
            .blocker_reason
            .as_deref()
            .map(simplify_reason_for_human)
            .unwrap_or_else(|| "需要确认具体业务做法后才能继续".to_string());
        return UserBlockerAlert {
            title: "有功能卡住，等您拍板后继续".to_string(),
            plain_explanation: format!(
                "【卡住的功能】「{}」（{}）\n\
                 【大白话原因】{}\n\
                 【怎么处理】请直接在下方输入框回复您的决定（例如选哪套方案或补充要求），团队收到后会马上继续开工。",
                stuck.title, stuck.task_id, reason
            ),
            task_id: Some(stuck.task_id.clone()),
            reason_kind: "needs_user_decision".to_string(),
        };
    }

    let missing_roles: Vec<&str> = [RoleKind::Architect, RoleKind::Coder, RoleKind::Reviewer]
        .iter()
        .filter(|role| {
            !slots
                .iter()
                .any(|s| s.enabled && s.role == **role && !s.state.is_unavailable())
        })
        .map(|role| role.label_zh())
        .collect();

    let cooling_count = slots
        .iter()
        .filter(|s| s.enabled && s.state == SlotRunState::QuotaCooling)
        .count();
    let error_count = slots
        .iter()
        .filter(|s| s.enabled && s.state == SlotRunState::Error)
        .count();

    let role_hint = if missing_roles.is_empty() {
        "当前编队中没有空闲可用的健康模型".to_string()
    } else {
        format!("目前缺少可用的{}模型", missing_roles.join("、"))
    };
    let cause_detail = if cooling_count > 0 && error_count > 0 {
        format!("有 {cooling_count} 个模型暂时触发了额度限流冷却，另有 {error_count} 个模型连续报错已停靠")
    } else if cooling_count > 0 {
        format!("有 {cooling_count} 个模型触发了账号额度或网关限流，正在冷却中")
    } else if error_count > 0 {
        format!("有 {error_count} 个模型连续执行失败，已被保护性停靠")
    } else {
        "所有启用中的模型目前处于暂停或外部接管状态".to_string()
    };

    UserBlockerAlert {
        title: "暂时没有可用的 AI 模型，研发已安全暂停".to_string(),
        plain_explanation: format!(
            "【卡住的原因】{role_hint}（{cause_detail}），所以自动研发已先安全停靠，没有丢失已完成进度。\n\
             【请您选择处理方式】\n\
             方式 1：在左侧「模型编队」中更换或新增一个可用模型，保存后点击「恢复」继续；\n\
             方式 2：如果只是临时限流，稍等片刻后点击顶栏「恢复」或发一句“继续”即可接力完成。"
        ),
        task_id: None,
        reason_kind: "all_slots_unavailable".to_string(),
    }
}

/// 将底层报错摘要做适度去噪和截断，避免直接把大段编译器堆栈抛给用户。
pub fn simplify_reason_for_human(raw: &str) -> String {
    let compact: String = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join("；");
    let cleaned = compact.trim();
    if cleaned.is_empty() {
        return "模块改动边界或方案细节存在冲突".to_string();
    }
    if cleaned.chars().count() > 140 {
        let mut s: String = cleaned.chars().take(137).collect();
        s.push_str("...");
        s
    } else {
        cleaned.to_string()
    }
}
