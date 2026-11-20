use crate::agy::runner::AgentTurnResponse;
use crate::agy::stream_parser::AgentErrorKind;
use crate::orchestrator::engine::{EngineEvent, OrchestratorEngine};
use crate::orchestrator::engine_turns::{
    build_completion_summary, handle_turn_error_on_slot, make_slot_message, make_system_message,
    merge_planned_tasks, parse_architect_output, parse_coder_output, parse_reviewer_output,
    FinishedTurn, TurnKind,
};
use crate::orchestrator::escalation::build_clarification_alert;
use crate::orchestrator::types::{
    AgentSlot, MessageBadge, RunPhase, SlotRunState, TaskStatus, UserBlockerAlert,
};

impl OrchestratorEngine {
    pub(super) fn handle_finished_turn(&mut self, done: FinishedTurn) {
        self.in_flight_turns = self.in_flight_turns.saturating_sub(1);

        let was_still_active = self
            .slot_cancel_txs
            .get(&done.slot_id)
            .map(|(tid, _)| *tid == done.turn_id)
            .unwrap_or(false);
        if was_still_active {
            self.slot_cancel_txs.remove(&done.slot_id);
        }

        if self.active_architect_turn == Some(done.turn_id) {
            self.active_architect_turn = None;
            self.architect_in_flight = false;
        }
        if self.active_reviewer_turn == Some(done.turn_id) {
            self.active_reviewer_turn = None;
            self.reviewer_in_flight = false;
        }

        // 已被 UpdateSlots 移除/停用或 Stop 取消的旧轮次，任务已在取消当下安全回收，直接忽略过期回调。
        if !was_still_active {
            return;
        }

        let Some(slot_idx) = self.slots.iter().position(|s| s.slot_id == done.slot_id) else {
            return;
        };

        let resp = match done.result {
            Ok(resp) => {
                self.settle_slot_turn_usage(
                    &done.slot_id,
                    resp.usage.clone(),
                    resp.peak_context_tokens,
                );
                let slot = &mut self.slots[slot_idx];
                if resp.switched_to_fresh_session {
                    slot.session_reused = false;
                }
                if let Some(cid) = &resp.conversation_id {
                    slot.conversation_id = Some(cid.clone());
                }
                if slot.state == SlotRunState::Working {
                    slot.state = SlotRunState::Idle;
                }
                slot.current_activity = None;
                slot.consecutive_failures = 0;
                let snap = slot.clone();
                self.emit_slot_state(&snap);
                if resp.switched_to_fresh_session {
                    self.emit_chat(make_system_message(
                        format!(
                            "{} (@{}) 检测到原历史会话上下文过大或网关响应迟缓，已自动切换为全新轻量会话继续执行。",
                            snap.display_name, snap.alias
                        ),
                        MessageBadge::Info,
                        None,
                    ));
                }
                resp
            }
            Err(err) => {
                self.settle_slot_interrupted_usage(
                    &done.slot_id,
                    err.partial_usage.clone(),
                    err.peak_context_tokens,
                );
                let is_canceled = err.kind == AgentErrorKind::Canceled;
                let (warn_msg, _quota) =
                    handle_turn_error_on_slot(&mut self.slots[slot_idx], &err);
                let snap = self.slots[slot_idx].clone();
                self.emit_slot_state(&snap);
                self.emit_chat(warn_msg);
                self.rollback_task_on_error(&done.slot_id, &done.kind, !is_canceled);
                return;
            }
        };

        let slot_snapshot = self.slots[slot_idx].clone();
        match done.kind {
            TurnKind::ArchitectInitialPlan
            | TurnKind::ArchitectUserInterjection { .. }
            | TurnKind::ArchitectIntervention { .. }
            | TurnKind::ArchitectFinalGate => {
                self.apply_architect_response(&slot_snapshot, &done.kind, &resp);
            }
            TurnKind::Coder { task_id, .. } => {
                self.apply_coder_response(&slot_snapshot, &task_id, &resp);
            }
            TurnKind::Reviewer { task_id } => {
                self.apply_reviewer_response(&slot_snapshot, &task_id, &resp);
            }
        }
    }

    fn rollback_task_on_error(&mut self, slot_id: &str, kind: &TurnKind, count_failure: bool) {
        match kind {
            TurnKind::Coder {
                task_id, is_rework, ..
            } => {
                if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == *task_id) {
                    if t.assigned_coder_slot_id.as_deref() == Some(slot_id)
                        || t.status == TaskStatus::Coding
                    {
                        if count_failure {
                            t.coder_failure_streak = t.coder_failure_streak.saturating_add(1);
                        }
                        t.assigned_coder_slot_id = None;
                        t.assigned_coder_label = None;
                        t.status = if *is_rework {
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
            TurnKind::ArchitectInitialPlan => {
                self.initial_plan_requested = false;
            }
            _ => {}
        }
    }

    fn apply_architect_response(
        &mut self,
        slot: &AgentSlot,
        kind: &TurnKind,
        resp: &AgentTurnResponse,
    ) {
        let parsed = match parse_architect_output(resp) {
            Ok(v) => {
                self.architect_parse_failures = 0;
                v
            }
            Err(e) => {
                self.architect_parse_failures = self.architect_parse_failures.saturating_add(1);
                if self.architect_parse_failures < 2 {
                    self.emit_chat(make_system_message(
                        format!(
                            "总架构师返回格式不完整（第 {} 次），正在自动重派一轮以确保结构化派单生效：{e}",
                            self.architect_parse_failures
                        ),
                        MessageBadge::Warning,
                        None,
                    ));
                    match kind {
                        TurnKind::ArchitectInitialPlan => {
                            self.initial_plan_requested = false;
                        }
                        TurnKind::ArchitectUserInterjection { notes } => {
                            for note in notes {
                                self.pending_user_notes.push(note.clone());
                            }
                        }
                        TurnKind::ArchitectIntervention { task_id, .. } => {
                            if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == *task_id) {
                                t.arch_arbitration_count =
                                    t.arch_arbitration_count.saturating_sub(1);
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                self.architect_parse_failures = 0;
                let alert = UserBlockerAlert {
                    title: "总架构师连续输出格式异常，需要您确认".to_string(),
                    plain_explanation: format!(
                        "【卡住的原因】总架构师连续 2 次没有按规定结构返回拆单/验收结果（{}）。\n\
                         【怎么处理】请点击顶栏「恢复」或在下方输入框回复“继续重试”，也可以在左侧切换一位更稳妥的总架构师模型后继续。",
                        e
                    ),
                    task_id: None,
                    reason_kind: "architect_parse_failed".to_string(),
                };
                self.emit_chat(make_system_message(
                    alert.plain_explanation.clone(),
                    MessageBadge::Warning,
                    None,
                ));
                self.set_phase(RunPhase::WaitingUser);
                self.emit_blocker_alert(alert);
                return;
            }
        };
        if matches!(kind, TurnKind::ArchitectUserInterjection { .. }) {
            self.initial_plan_requested = true;
        }

        if !parsed.architecture_notes.trim().is_empty() {
            self.architecture_notes = parsed.architecture_notes.clone();
        }
        if !parsed.verification_commands.is_empty() {
            self.verification_commands = parsed.verification_commands.clone();
        }

        match parsed.action.as_str() {
            "FINAL_ACCEPT" => {
                self.emit_chat(make_slot_message(
                    slot,
                    parsed.chat_message.clone(),
                    MessageBadge::FinalAccept,
                    None,
                    Vec::new(),
                    resp,
                    Vec::new(),
                ));
                let summary = build_completion_summary(
                    &self.cfg.workspace_dir.to_string_lossy(),
                    &self.cfg.goal,
                    self.started_at.elapsed().as_secs(),
                    &self.tasks,
                    &self.verified_commands,
                    &self.slots,
                    &self.turn_counts,
                    parsed.chat_message,
                );
                self.set_phase(RunPhase::Completed);
                let _ = self.event_tx.send(EngineEvent::Completed(summary));
            }
            "NEED_USER_CLARIFICATION" => {
                let task_hint = match kind {
                    TurnKind::ArchitectIntervention { task_id, .. } => Some(task_id.clone()),
                    _ => None,
                };
                if let Some(ref tid) = task_hint {
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == *tid) {
                        t.status = TaskStatus::NeedsUserDecision;
                    }
                    self.emit_tasks();
                }
                let alert = build_clarification_alert(
                    &parsed.chat_message,
                    parsed.user_plain_explanation.as_deref(),
                    task_hint.clone(),
                );
                let display_chat = match parsed
                    .user_plain_explanation
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(plain) if parsed.chat_message.trim().is_empty() => plain.to_string(),
                    Some(plain) if !parsed.chat_message.contains(plain) => {
                        format!("{plain}\n\n（补充说明：{}）", parsed.chat_message.trim())
                    }
                    _ => parsed.chat_message.clone(),
                };
                self.emit_chat(make_slot_message(
                    slot,
                    display_chat,
                    MessageBadge::Warning,
                    task_hint,
                    Vec::new(),
                    resp,
                    Vec::new(),
                ));
                self.set_phase(RunPhase::WaitingUser);
                self.emit_blocker_alert(alert);
            }
            _ => {
                let added = merge_planned_tasks(&mut self.tasks, parsed.tasks);

                if let TurnKind::ArchitectIntervention { task_id, .. } = kind {
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == *task_id) {
                        if t.status == TaskStatus::BlockedByArch
                            || (t.status == TaskStatus::ReworkNeeded
                                && t.rework_count >= self.cfg.max_rework_rounds)
                        {
                            t.status = TaskStatus::Pending;
                            t.blocker_reason = None;
                            t.rework_count = 0;
                        }
                    }
                }
                if matches!(kind, TurnKind::ArchitectUserInterjection { .. }) {
                    for t in &mut self.tasks {
                        if t.status != TaskStatus::Approved {
                            t.arch_arbitration_count = 0;
                        }
                        if matches!(
                            t.status,
                            TaskStatus::BlockedByArch | TaskStatus::NeedsUserDecision
                        ) {
                            t.status = TaskStatus::Pending;
                            t.blocker_reason = None;
                        }
                    }
                }

                let will_complete =
                    (matches!(kind, TurnKind::ArchitectFinalGate) || self.tasks.is_empty())
                        && added == 0;
                let chat_summary = parsed.chat_message.clone();
                self.emit_chat(make_slot_message(
                    slot,
                    parsed.chat_message,
                    if will_complete {
                        MessageBadge::FinalAccept
                    } else {
                        MessageBadge::Plan
                    },
                    None,
                    Vec::new(),
                    resp,
                    Vec::new(),
                ));
                if will_complete {
                    let final_note = if !chat_summary.trim().is_empty() {
                        chat_summary
                    } else {
                        "所有子任务已完成审查并通过最终验收。".to_string()
                    };
                    let summary = build_completion_summary(
                        &self.cfg.workspace_dir.to_string_lossy(),
                        &self.cfg.goal,
                        self.started_at.elapsed().as_secs(),
                        &self.tasks,
                        &self.verified_commands,
                        &self.slots,
                        &self.turn_counts,
                        final_note,
                    );
                    self.set_phase(RunPhase::Completed);
                    let _ = self.event_tx.send(EngineEvent::Completed(summary));
                } else {
                    self.set_phase(RunPhase::Running);
                    self.emit_tasks();
                }
            }
        }
    }

    fn apply_coder_response(
        &mut self,
        slot: &AgentSlot,
        task_id: &str,
        resp: &AgentTurnResponse,
    ) {
        let parsed = match parse_coder_output(resp) {
            Ok(v) => v,
            Err(e) => {
                self.emit_chat(make_system_message(
                    e,
                    MessageBadge::Warning,
                    Some(task_id.to_string()),
                ));
                if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == task_id) {
                    t.coder_failure_streak = t.coder_failure_streak.saturating_add(1);
                    t.status = TaskStatus::ReworkNeeded;
                }
                self.emit_tasks();
                return;
            }
        };

        let badge = if parsed.status == "BLOCKED_BY_ARCH" {
            MessageBadge::Blocked
        } else {
            MessageBadge::Submit
        };

        if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == task_id) {
            t.coder_failure_streak = 0;
            t.last_coder_summary = Some(parsed.chat_message.clone());
            t.last_modified_files = parsed.modified_files.clone();
            t.last_self_check = Some(parsed.self_check_result.clone());
            if parsed.status == "BLOCKED_BY_ARCH" {
                t.status = TaskStatus::BlockedByArch;
                t.blocker_reason = parsed.blocker_reason.clone();
            } else {
                t.status = TaskStatus::PendingReview;
                t.blocker_reason = None;
            }
        }

        self.emit_chat(make_slot_message(
            slot,
            parsed.chat_message,
            badge,
            Some(task_id.to_string()),
            parsed.modified_files,
            resp,
            Vec::new(),
        ));
        self.emit_tasks();
    }

    fn apply_reviewer_response(
        &mut self,
        slot: &AgentSlot,
        task_id: &str,
        resp: &AgentTurnResponse,
    ) {
        let parsed = match parse_reviewer_output(resp) {
            Ok(v) => v,
            Err(e) => {
                self.emit_chat(make_system_message(
                    e,
                    MessageBadge::Warning,
                    Some(task_id.to_string()),
                ));
                if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == task_id) {
                    t.status = TaskStatus::PendingReview;
                }
                self.emit_tasks();
                return;
            }
        };

        for cmd in &parsed.commands_verified {
            if !cmd.trim().is_empty() {
                self.verified_commands.push(cmd.clone());
            }
        }

        let approved = parsed.verdict == "APPROVED";
        if let Some(t) = self.tasks.iter_mut().find(|t| t.task_id == task_id) {
            t.last_review_note = Some(parsed.chat_message.clone());
            t.last_review_issues = parsed.issues.clone();
            if approved {
                t.status = TaskStatus::Approved;
                t.arch_arbitration_count = 0;
            } else {
                t.rework_count = t.rework_count.saturating_add(1);
                t.status = TaskStatus::ReworkNeeded;
            }
        }

        self.emit_chat(make_slot_message(
            slot,
            parsed.chat_message,
            if approved {
                MessageBadge::Approved
            } else {
                MessageBadge::Rework
            },
            Some(task_id.to_string()),
            Vec::new(),
            resp,
            parsed.issues,
        ));
        self.emit_tasks();
    }
}
