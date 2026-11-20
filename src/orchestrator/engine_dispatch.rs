use crate::orchestrator::engine::OrchestratorEngine;
use crate::orchestrator::engine_turns::{make_system_message, TurnKind};
use crate::orchestrator::escalation::{
    build_all_slots_unavailable_alert, evaluate_blocked_task_escalation, EscalationDecision,
    MAX_ARCH_ARBITRATION_ROUNDS,
};
use crate::orchestrator::priority_pool::{
    select_slot_for_new_task, select_slot_for_rework, SlotSelection,
};
use crate::orchestrator::types::{MessageBadge, RoleKind, RunPhase, TaskStatus};
use crate::orchestrator::workspace_guard::{dependencies_satisfied, has_active_path_conflict};

impl OrchestratorEngine {
    pub(super) fn dispatch_next_work(&mut self) {
        if matches!(
            self.phase,
            RunPhase::Paused
                | RunPhase::WaitingUser
                | RunPhase::Completed
                | RunPhase::Stopped
                | RunPhase::Failed
        ) {
            return;
        }

        // 1. User interjections: wake Architect immediately whenever Architect is idle.
        if !self.pending_user_notes.is_empty() && !self.architect_in_flight {
            let notes = std::mem::take(&mut self.pending_user_notes);
            self.spawn_architect_turn(TurnKind::ArchitectUserInterjection { notes });
        }

        // 2. Initial planning turn.
        if !self.initial_plan_requested && !self.architect_in_flight {
            self.initial_plan_requested = true;
            self.spawn_architect_turn(TurnKind::ArchitectInitialPlan);
            return;
        }

        // 3. Two-tier blocker escalation: wake idle Architect immediately without waiting
        //    for other coders to finish; escalate to user popup only on Tier 2.
        if !self.architect_in_flight {
            match evaluate_blocked_task_escalation(&mut self.tasks, self.cfg.max_rework_rounds) {
                EscalationDecision::ArbitrateByArchitect {
                    task_id,
                    reason,
                    round,
                } => {
                    self.emit_tasks();
                    self.emit_chat(make_system_message(
                        format!(
                            "子任务 {task_id} 遇到阻塞，已自动唤醒总架构师执行第 {round}/{MAX_ARCH_ARBITRATION_ROUNDS} 次内部技术裁决（无需人工打扰）。"
                        ),
                        MessageBadge::Info,
                        Some(task_id.clone()),
                    ));
                    self.spawn_architect_turn(TurnKind::ArchitectIntervention {
                        task_id,
                        reason,
                        round,
                    });
                }
                EscalationDecision::EscalateToUser { task_id, alert } => {
                    self.emit_tasks();
                    self.emit_chat(make_system_message(
                        alert.plain_explanation.clone(),
                        MessageBadge::Warning,
                        task_id,
                    ));
                    self.set_phase(RunPhase::WaitingUser);
                    self.emit_blocker_alert(alert);
                    return;
                }
                EscalationDecision::None => {}
            }
        }

        // 4. Reviewer dispatch (strictly serialized: max 1 reviewer at a time for test isolation).
        if !self.reviewer_in_flight && !self.architect_in_flight {
            if let Some(task_idx) = self
                .tasks
                .iter()
                .position(|t| t.status == TaskStatus::PendingReview)
            {
                if let SlotSelection::Selected { slot_id, .. } = select_slot_for_new_task(
                    RoleKind::Reviewer,
                    None,
                    &self.slots,
                    self.cfg.dispatch_mode,
                ) {
                    self.spawn_reviewer_turn(task_idx, slot_id);
                }
            }
        }

        // 5. Coder dispatch: ReworkNeeded has higher priority than brand-new Pending tasks.
        if !self.architect_in_flight {
            self.dispatch_coder_tasks();
        }

        // 6. Final Gate when every task is Approved and nothing is running.
        if self.in_flight_turns == 0
            && !self.tasks.is_empty()
            && self.tasks.iter().all(|t| t.status == TaskStatus::Approved)
        {
            self.set_phase(RunPhase::FinalGate);
            self.spawn_architect_turn(TurnKind::ArchitectFinalGate);
            return;
        }

        // 7. Stall / all-slots-unavailable escalation when nothing is running.
        if self.in_flight_turns == 0 && !self.tasks.is_empty() {
            let has_unresolved = self.tasks.iter().any(|t| t.status != TaskStatus::Approved);
            if has_unresolved {
                let alert = build_all_slots_unavailable_alert(&self.slots, &self.tasks);
                self.set_phase(RunPhase::WaitingUser);
                self.emit_chat(make_system_message(
                    alert.plain_explanation.clone(),
                    MessageBadge::Warning,
                    alert.task_id.clone(),
                ));
                self.emit_blocker_alert(alert);
            }
        }
    }

    fn dispatch_coder_tasks(&mut self) {
        // Pass A: ReworkNeeded tasks first (sticky to original coder).
        let rework_indices: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.status == TaskStatus::ReworkNeeded)
            .map(|(i, _)| i)
            .collect();

        for idx in rework_indices {
            if !dependencies_satisfied(&self.tasks[idx], &self.tasks) {
                continue;
            }
            if has_active_path_conflict(&self.tasks[idx], &self.tasks).is_some() {
                continue;
            }
            let sel = select_slot_for_rework(
                self.tasks[idx].last_coder_slot_id.as_deref(),
                self.tasks[idx].coder_failure_streak,
                &self.slots,
                self.cfg.dispatch_mode,
            );
            match sel {
                SlotSelection::Selected {
                    slot_id,
                    is_handoff,
                } => {
                    self.spawn_coder_turn(idx, slot_id, true, is_handoff);
                }
                SlotSelection::WaitForBusyPrimary { .. } | SlotSelection::AllBusy => break,
                SlotSelection::WaitForOriginalCoder { .. } | SlotSelection::NoneAvailable => {}
            }
        }

        // Pass B: Ready Pending / WaitingPathLock tasks.
        let ready_indices: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t.status, TaskStatus::Pending | TaskStatus::WaitingPathLock))
            .map(|(i, _)| i)
            .collect();

        let mut tasks_changed = false;
        for idx in ready_indices {
            if !dependencies_satisfied(&self.tasks[idx], &self.tasks) {
                continue;
            }
            if has_active_path_conflict(&self.tasks[idx], &self.tasks).is_some() {
                if self.tasks[idx].status != TaskStatus::WaitingPathLock {
                    self.tasks[idx].status = TaskStatus::WaitingPathLock;
                    tasks_changed = true;
                }
                continue;
            }

            let sel = select_slot_for_new_task(
                RoleKind::Coder,
                self.tasks[idx].preferred_coder_alias.as_deref(),
                &self.slots,
                self.cfg.dispatch_mode,
            );
            match sel {
                SlotSelection::Selected { slot_id, .. } => {
                    self.spawn_coder_turn(idx, slot_id, false, false);
                }
                SlotSelection::WaitForBusyPrimary { .. } | SlotSelection::AllBusy => break,
                SlotSelection::WaitForOriginalCoder { .. } | SlotSelection::NoneAvailable => {}
            }
        }
        if tasks_changed {
            self.emit_tasks();
        }
    }
}
