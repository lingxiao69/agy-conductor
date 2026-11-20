use crate::agy::gateway_wildcards::sanitize_wildcard_model_id;
use crate::billing::SlotUsageStats;
use crate::orchestrator::engine::OrchestratorEngine;
use crate::orchestrator::engine_turns::{make_system_message, TurnKind};
use crate::orchestrator::types::{AgentSlot, MessageBadge, RunPhase, SlotRunState, TaskStatus};
use std::collections::HashMap;
use tokio::sync::watch;

pub(super) struct ActiveSlotTurn {
    pub turn_id: u64,
    pub kind: TurnKind,
    pub cancel_tx: watch::Sender<bool>,
}

impl OrchestratorEngine {
    /// 统一回滚未完成（无槽位、被取消、解析失败或异常退出）的轮次状态：
    /// - `ArchitectInitialPlan`：无任务时复位 `initial_plan_requested` 避免假死；
    /// - `ArchitectUserInterjection`：非 `Stopped` 状态下按原顺序头插回 `pending_user_notes` 队首；
    /// - `ArchitectIntervention`：回退 `arch_arbitration_count`，避免白扣静默仲裁次数；
    /// - `Coder` / `Reviewer`：安全回退至 `ReworkNeeded` / `Pending` / `PendingReview`。
    pub(super) fn rollback_interrupted_turn(
        &mut self,
        slot_id: &str,
        kind: &TurnKind,
        count_failure: bool,
    ) {
        match kind {
            TurnKind::ArchitectInitialPlan => {
                if self.tasks.is_empty() {
                    self.initial_plan_requested = false;
                }
            }
            TurnKind::ArchitectUserInterjection { notes } => {
                if self.phase != RunPhase::Stopped && !notes.is_empty() {
                    let mut restored = notes.clone();
                    restored.append(&mut self.pending_user_notes);
                    self.pending_user_notes = restored;
                }
                if self.tasks.is_empty() {
                    self.initial_plan_requested = false;
                }
            }
            TurnKind::ArchitectIntervention { task_id, .. } => {
                if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == *task_id) {
                    t.arch_arbitration_count = t.arch_arbitration_count.saturating_sub(1);
                }
                self.emit_tasks();
            }
            TurnKind::ArchitectFinalGate => {}
            TurnKind::Coder {
                task_id, is_rework, ..
            } => {
                if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == *task_id) {
                    let slot_matches = slot_id.is_empty()
                        || t.assigned_coder_slot_id.is_none()
                        || t.assigned_coder_slot_id.as_deref() == Some(slot_id);
                    if t.status == TaskStatus::Coding && slot_matches {
                        if count_failure {
                            t.coder_failure_streak = t.coder_failure_streak.saturating_add(1);
                        }
                        t.assigned_coder_slot_id = None;
                        t.assigned_coder_label = None;
                        t.status = if *is_rework || t.rework_count > 0 {
                            TaskStatus::ReworkNeeded
                        } else {
                            TaskStatus::Pending
                        };
                    }
                }
                self.emit_tasks();
            }
            TurnKind::Reviewer { task_id } => {
                if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == *task_id) {
                    if t.status == TaskStatus::InReview {
                        t.status = TaskStatus::PendingReview;
                    }
                }
                self.emit_tasks();
            }
        }
    }

    /// 主动取消指定槽位的在途轮次、清理角色占用标志、结算中断用量并执行统一回滚。
    pub(super) fn cancel_and_rollback_slot(
        &mut self,
        slot_id: &str,
        reset_slot_to_idle: bool,
    ) -> Option<TurnKind> {
        let active = self.slot_cancel_txs.remove(slot_id)?;
        let _ = active.cancel_tx.send(true);

        if self.active_architect_turn == Some(active.turn_id) {
            self.active_architect_turn = None;
            self.architect_in_flight = false;
        }
        if self.active_reviewer_turn == Some(active.turn_id) {
            self.active_reviewer_turn = None;
            self.reviewer_in_flight = false;
        }

        self.settle_slot_interrupted_usage(slot_id, None, 0);
        self.rollback_interrupted_turn(slot_id, &active.kind, false);

        if reset_slot_to_idle {
            if let Some(slot) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) {
                if slot.state == SlotRunState::Working {
                    slot.state = SlotRunState::Idle;
                    slot.current_activity = None;
                    let snap = slot.clone();
                    self.emit_slot_state(&snap);
                }
            }
        }

        Some(active.kind)
    }

    /// 取消某个子任务当前绑定的所有在途 Coder / Reviewer 轮次（用于契约变更重置前），
    /// 结算用量并释放对应槽位状态，不覆盖任务的新状态。
    pub(super) fn cancel_active_turns_for_task(&mut self, task_id: &str) -> Vec<String> {
        let matched_slots: Vec<String> = self
            .slot_cancel_txs
            .iter()
            .filter_map(|(sid, active)| match &active.kind {
                TurnKind::Coder { task_id: tid, .. } | TurnKind::Reviewer { task_id: tid }
                    if tid == task_id =>
                {
                    Some(sid.clone())
                }
                _ => None,
            })
            .collect();

        let mut cancelled_slots = Vec::new();
        for sid in matched_slots {
            if let Some(active) = self.slot_cancel_txs.remove(&sid) {
                let _ = active.cancel_tx.send(true);
                if self.active_reviewer_turn == Some(active.turn_id) {
                    self.active_reviewer_turn = None;
                    self.reviewer_in_flight = false;
                }
                self.settle_slot_interrupted_usage(&sid, None, 0);
                if let Some(slot) = self.slots.iter_mut().find(|s| s.slot_id == sid) {
                    let label = format!("{} (@{})", slot.display_name, slot.alias);
                    let snap = if slot.state == SlotRunState::Working {
                        slot.state = SlotRunState::Idle;
                        slot.current_activity = None;
                        Some(slot.clone())
                    } else {
                        None
                    };
                    if let Some(snap) = snap {
                        self.emit_slot_state(&snap);
                    }
                    cancelled_slots.push(label);
                }
            }
        }
        cancelled_slots
    }

    /// 外部 CLI 接管槽位前，若存在 TOCTOU 竞态导致的在途后台轮次，先取消结算并回滚再标记接管。
    pub(super) fn attach_slot_external_cli(&mut self, slot_id: &str) {
        let had_active_turn = self.cancel_and_rollback_slot(slot_id, false).is_some();
        if let Some(slot) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) {
            slot.state = SlotRunState::ExternalCliAttached;
            slot.current_activity = Some("已由外部 CLI 接管".to_string());
            let snap = slot.clone();
            self.emit_slot_state(&snap);
            if had_active_turn {
                self.emit_chat(make_system_message(
                    format!(
                        "{} (@{}) 已切换为外部 CLI 接管，已安全终止其后台在途轮次并回收任务状态，避免并发读写同一会话。",
                        snap.display_name, snap.alias
                    ),
                    MessageBadge::Info,
                    None,
                ));
            }
        }
    }

    pub(super) fn release_slot_external_cli(&mut self, slot_id: &str) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) {
            if slot.state == SlotRunState::ExternalCliAttached {
                slot.state = SlotRunState::Idle;
                slot.current_activity = None;
                let snap = slot.clone();
                self.emit_slot_state(&snap);
                self.emit_chat(make_system_message(
                    format!("{} 已结束外部 CLI 接管，回到自动编队协同。", snap.display_name),
                    MessageBadge::Info,
                    None,
                ));
            }
        }
    }

    pub(super) fn apply_updated_slots(&mut self, updated: Vec<AgentSlot>) {
        let normalized_updated: Vec<AgentSlot> = updated
            .into_iter()
            .map(|mut s| {
                s.model_id = sanitize_wildcard_model_id(&s.model_id);
                s.priority = s.normalized_priority();
                s
            })
            .collect();

        let incoming_map: HashMap<String, AgentSlot> = normalized_updated
            .iter()
            .cloned()
            .map(|s| (s.slot_id.clone(), s))
            .collect();

        let existing_snapshot: Vec<AgentSlot> = self.slots.clone();
        let mut cancelled_labels = Vec::new();
        let mut rolled_back_tasks = Vec::new();

        for existing in &existing_snapshot {
            let should_cancel = match incoming_map.get(&existing.slot_id) {
                None => true,
                Some(inc) => {
                    !inc.enabled
                        || inc.role != existing.role
                        || inc.model_id != existing.model_id
                }
            };
            if !should_cancel {
                continue;
            }

            let mut affected = false;
            if let Some(cancelled_kind) = self.cancel_and_rollback_slot(&existing.slot_id, false) {
                affected = true;
                match cancelled_kind {
                    TurnKind::Coder { task_id, .. } | TurnKind::Reviewer { task_id } => {
                        if !rolled_back_tasks.contains(&task_id) {
                            rolled_back_tasks.push(task_id);
                        }
                    }
                    _ => {}
                }
            }

            let mut orphan_task_touched = false;
            for task in &mut self.tasks {
                if task.status == TaskStatus::Coding
                    && task.assigned_coder_slot_id.as_deref() == Some(existing.slot_id.as_str())
                {
                    task.status = if task.rework_count > 0 {
                        TaskStatus::ReworkNeeded
                    } else {
                        TaskStatus::Pending
                    };
                    task.assigned_coder_slot_id = None;
                    task.assigned_coder_label = None;
                    if !rolled_back_tasks.contains(&task.task_id) {
                        rolled_back_tasks.push(task.task_id.clone());
                    }
                    affected = true;
                    orphan_task_touched = true;
                }
                if task.last_coder_slot_id.as_deref() == Some(existing.slot_id.as_str()) {
                    task.last_coder_slot_id = None;
                }
            }
            if orphan_task_touched {
                self.emit_tasks();
            }

            if affected {
                cancelled_labels.push(format!("{} (@{})", existing.display_name, existing.alias));
            }
        }

        self.slots
            .retain(|s| incoming_map.contains_key(&s.slot_id));

        for mut incoming in normalized_updated {
            if let Some(existing) = self
                .slots
                .iter_mut()
                .find(|s| s.slot_id == incoming.slot_id)
            {
                let identity_changed =
                    existing.model_id != incoming.model_id || existing.role != incoming.role;
                existing.priority = incoming.priority;
                existing.enabled = incoming.enabled;
                existing.model_id = incoming.model_id;
                existing.role = incoming.role;
                existing.display_name = incoming.display_name;
                existing.alias = incoming.alias;
                if identity_changed {
                    existing.conversation_id = None;
                    existing.session_reused = false;
                    existing.usage = SlotUsageStats::for_model(&existing.model_id);
                    self.usage_ledgers.remove(&existing.slot_id);
                }
                if (!existing.enabled || identity_changed) && existing.state == SlotRunState::Working
                {
                    existing.state = SlotRunState::Idle;
                    existing.current_activity = None;
                    let snap = existing.clone();
                    self.emit_slot_state(&snap);
                } else if identity_changed {
                    let snap = existing.clone();
                    self.emit_slot_state(&snap);
                }
            } else {
                incoming.usage = SlotUsageStats::for_model(&incoming.model_id);
                incoming.consecutive_failures = 0;
                if incoming.state != SlotRunState::ExternalCliAttached {
                    incoming.state = SlotRunState::Idle;
                    incoming.current_activity = None;
                }
                let snap = incoming.clone();
                self.slots.push(incoming);
                self.emit_slot_state(&snap);
            }
        }

        if !cancelled_labels.is_empty() {
            let task_note = if rolled_back_tasks.is_empty() {
                String::new()
            } else {
                format!(
                    "，已将子任务 {} 回收给其他健康模型继续接力",
                    rolled_back_tasks.join("、")
                )
            };
            self.emit_chat(make_system_message(
                format!(
                    "编队变更：已立即终止停用/移除槽位 {} 的后台进程{task_note}。",
                    cancelled_labels.join("、")
                ),
                MessageBadge::Info,
                None,
            ));
        }
    }
}
