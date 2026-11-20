use crate::agy::runner::AgentTurnRequest;
use crate::orchestrator::engine::OrchestratorEngine;
use crate::orchestrator::engine_roster::ActiveSlotTurn;
use crate::orchestrator::engine_turns::{make_system_message, FinishedTurn, TurnKind};
use crate::orchestrator::escalation::{
    build_all_slots_unavailable_alert, MAX_ARCH_ARBITRATION_ROUNDS,
};
use crate::orchestrator::priority_pool::{select_slot_for_new_task, SlotSelection};
use crate::orchestrator::prompt_builder::{
    build_architect_arbitration_directive, build_architect_final_gate_prompt,
    build_architect_planning_prompt, build_coder_prompt, build_reviewer_prompt,
    build_user_interjection_directive, pick_role_alias,
};
use crate::orchestrator::types::{MessageBadge, RoleKind, RunPhase, SlotRunState, TaskStatus};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

impl OrchestratorEngine {
    pub(super) fn spawn_architect_turn(&mut self, kind: TurnKind) -> bool {
        let SlotSelection::Selected { slot_id, .. } = select_slot_for_new_task(
            RoleKind::Architect,
            None,
            &self.slots,
            self.cfg.dispatch_mode,
        ) else {
            self.rollback_interrupted_turn("", &kind, false);
            let alert = build_all_slots_unavailable_alert(&self.slots, &self.tasks);
            self.set_phase(RunPhase::WaitingUser);
            self.emit_chat(make_system_message(
                alert.plain_explanation.clone(),
                MessageBadge::Warning,
                alert.task_id.clone(),
            ));
            self.emit_blocker_alert(alert);
            return false;
        };

        let arch_alias = self
            .slots
            .iter()
            .find(|s| s.slot_id == slot_id)
            .map(|s| s.alias.clone())
            .unwrap_or_else(|| "architect".to_string());

        let (prompt, activity) = match &kind {
            TurnKind::ArchitectInitialPlan => {
                self.initial_plan_requested = true;
                // 派单给总架构师瞬间（<100ms）立刻在主群聊与槽位状态给出清晰可见的反馈
                self.emit_chat(make_system_message(
                    format!(
                        "已将长期目标第一时间派发给总架构师 @{arch_alias}，正在快速勘察代码结构并拆解子任务清单，随后将按优先级依次调度程序员与审查员..."
                    ),
                    MessageBadge::Info,
                    None,
                ));
                (
                    build_architect_planning_prompt(
                        &self.cfg.goal,
                        &self.cfg.workspace_dir.to_string_lossy(),
                        &self.slots,
                        &self.chat_history,
                        &self.tasks,
                        None,
                    ),
                    "正在接单连接模型并勘察项目...".to_string(),
                )
            }
            TurnKind::ArchitectUserInterjection { notes } => {
                self.initial_plan_requested = true;
                let directive = build_user_interjection_directive(notes);
                (
                    build_architect_planning_prompt(
                        &self.cfg.goal,
                        &self.cfg.workspace_dir.to_string_lossy(),
                        &self.slots,
                        &self.chat_history,
                        &self.tasks,
                        Some(&directive),
                    ),
                    "正在响应您的指导意见并动态调整方案...".to_string(),
                )
            }
            TurnKind::ArchitectIntervention {
                task_id,
                reason,
                round,
            } => {
                let directive = build_architect_arbitration_directive(
                    task_id,
                    reason,
                    *round,
                    MAX_ARCH_ARBITRATION_ROUNDS,
                );
                (
                    build_architect_planning_prompt(
                        &self.cfg.goal,
                        &self.cfg.workspace_dir.to_string_lossy(),
                        &self.slots,
                        &self.chat_history,
                        &self.tasks,
                        Some(&directive),
                    ),
                    format!("正在内部裁决子任务 {task_id} 阻塞（第 {round} 次）..."),
                )
            }
            TurnKind::ArchitectFinalGate => {
                self.set_phase(RunPhase::FinalGate);
                (
                    build_architect_final_gate_prompt(
                        &self.cfg.goal,
                        &self.cfg.workspace_dir.to_string_lossy(),
                        &self.slots,
                        &self.tasks,
                        &self.chat_history,
                        &self.verified_commands,
                    ),
                    "正在执行全局终验门禁核验...".to_string(),
                )
            }
            _ => return false,
        };

        self.architect_in_flight = true;
        self.spawn_slot_turn(
            slot_id,
            self.cfg.schemas.architect.clone(),
            prompt,
            activity,
            kind,
        );
        true
    }

    pub(super) fn spawn_coder_turn(
        &mut self,
        task_idx: usize,
        slot_id: String,
        is_rework: bool,
        is_handoff: bool,
    ) {
        let Some(slot_snapshot) = self.slots.iter().find(|s| s.slot_id == slot_id).cloned() else {
            return;
        };
        let reviewer_alias = pick_role_alias(&self.slots, RoleKind::Reviewer, "reviewer");
        let architect_alias = pick_role_alias(&self.slots, RoleKind::Architect, "architect");

        let task = &mut self.tasks[task_idx];
        task.status = TaskStatus::Coding;
        task.assigned_coder_slot_id = Some(slot_snapshot.slot_id.clone());
        task.assigned_coder_label = Some(format!(
            "{} (P{})",
            slot_snapshot.alias,
            slot_snapshot.normalized_priority()
        ));
        task.last_coder_slot_id = Some(slot_snapshot.slot_id.clone());
        let task_snapshot = task.clone();
        self.emit_tasks();

        if is_handoff {
            self.emit_chat(make_system_message(
                format!(
                    "子任务 {} 已无缝转交至 @{} (P{}) 接力修复。",
                    task_snapshot.task_id,
                    slot_snapshot.alias,
                    slot_snapshot.normalized_priority()
                ),
                MessageBadge::Info,
                Some(task_snapshot.task_id.clone()),
            ));
        }

        let prompt = build_coder_prompt(
            &self.cfg.goal,
            &self.architecture_notes,
            &task_snapshot,
            &slot_snapshot,
            &reviewer_alias,
            &architect_alias,
            self.shared_build_lock,
            is_rework,
            is_handoff,
        );

        let verb = if is_rework { "修复" } else { "编码" };
        self.spawn_slot_turn(
            slot_id,
            self.cfg.schemas.coder.clone(),
            prompt,
            format!("正在{} {} ...", verb, task_snapshot.task_id),
            TurnKind::Coder {
                task_id: task_snapshot.task_id,
                is_rework,
                is_handoff,
            },
        );
    }

    pub(super) fn spawn_reviewer_turn(&mut self, task_idx: usize, slot_id: String) {
        let task = &mut self.tasks[task_idx];
        task.status = TaskStatus::InReview;
        let task_snapshot = task.clone();
        self.emit_tasks();

        let coder_alias = task_snapshot
            .last_coder_slot_id
            .as_deref()
            .and_then(|sid| self.slots.iter().find(|s| s.slot_id == sid))
            .map(|s| s.alias.clone())
            .unwrap_or_else(|| "coder".to_string());

        let prompt = build_reviewer_prompt(
            &self.cfg.goal,
            &self.architecture_notes,
            &self.verification_commands,
            &task_snapshot,
            &coder_alias,
        );

        self.reviewer_in_flight = true;
        self.spawn_slot_turn(
            slot_id,
            self.cfg.schemas.reviewer.clone(),
            prompt,
            format!("正在独立审查 {} ...", task_snapshot.task_id),
            TurnKind::Reviewer {
                task_id: task_snapshot.task_id,
            },
        );
    }

    pub(super) fn spawn_slot_turn(
        &mut self,
        slot_id: String,
        schema_path: PathBuf,
        prompt: String,
        initial_activity: String,
        kind: TurnKind,
    ) {
        let Some(slot) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) else {
            return;
        };
        slot.state = SlotRunState::Working;
        slot.current_activity = Some(initial_activity);
        let req = AgentTurnRequest {
            slot_id: slot.slot_id.clone(),
            model_id: slot.model_id.clone(),
            workspace_dir: self.cfg.workspace_dir.clone(),
            schema_path,
            prompt,
            conversation_id: slot.conversation_id.clone(),
            permission_mode: self.cfg.permission_mode,
            enable_sandbox: self.cfg.enable_sandbox,
            timeout_secs: self.cfg.turn_timeout_secs,
        };
        let slot_snap = slot.clone();
        self.emit_slot_state(&slot_snap);

        self.turn_seq += 1;
        let turn_id = self.turn_seq;
        match &kind {
            TurnKind::ArchitectInitialPlan
            | TurnKind::ArchitectUserInterjection { .. }
            | TurnKind::ArchitectIntervention { .. }
            | TurnKind::ArchitectFinalGate => {
                self.active_architect_turn = Some(turn_id);
            }
            TurnKind::Reviewer { .. } => {
                self.active_reviewer_turn = Some(turn_id);
            }
            TurnKind::Coder { .. } => {}
        }

        let (slot_cancel_tx, mut slot_cancel_rx) = watch::channel(false);
        self.slot_cancel_txs.insert(
            slot_id.clone(),
            ActiveSlotTurn {
                turn_id,
                kind: kind.clone(),
                cancel_tx: slot_cancel_tx,
            },
        );

        self.in_flight_turns += 1;
        *self.turn_counts.entry(slot_id.clone()).or_insert(0) += 1;

        let runner = Arc::clone(&self.runner);
        let act_tx = self.activity_tx.clone();
        let done_tx = self.turn_done_tx.clone();
        let mut global_cancel_rx = self.cancel_tx.subscribe();

        tokio::spawn(async move {
            let (merged_tx, merged_rx) = watch::channel(false);
            let cancel_bridge = tokio::spawn(async move {
                if *global_cancel_rx.borrow() || *slot_cancel_rx.borrow() {
                    let _ = merged_tx.send(true);
                    return;
                }
                let mut global_open = true;
                let mut slot_open = true;
                while global_open || slot_open {
                    tokio::select! {
                        res = global_cancel_rx.changed(), if global_open => {
                            match res {
                                Ok(()) if *global_cancel_rx.borrow() => {
                                    let _ = merged_tx.send(true);
                                    return;
                                }
                                Ok(()) => {}
                                Err(_) => global_open = false,
                            }
                        }
                        res = slot_cancel_rx.changed(), if slot_open => {
                            match res {
                                Ok(()) if *slot_cancel_rx.borrow() => {
                                    let _ = merged_tx.send(true);
                                    return;
                                }
                                Ok(()) => {}
                                Err(_) => slot_open = false,
                            }
                        }
                    }
                }
            });
            let result = runner.run_turn(req, act_tx, merged_rx).await;
            cancel_bridge.abort();
            let _ = done_tx.send(FinishedTurn {
                turn_id,
                slot_id,
                kind,
                result,
            });
        });
    }
}
