use agy_conductor::agy::runner::{
    AgentActivityUpdate, AgentTurnRequest, AgentTurnResponse, AgyRunner, TurnFuture,
};
use agy_conductor::agy::stream_parser::TokenUsage;
use agy_conductor::billing::SlotUsageStats;
use agy_conductor::orchestrator::engine::{
    EngineConfig, EngineControl, EngineEvent, OrchestratorEngine,
};
use agy_conductor::orchestrator::schemas::{write_role_schemas, RoleSchemaFiles};
use agy_conductor::orchestrator::types::{
    AgentSlot, PermissionMode, RoleKind, RunPhase, SameRoleDispatchMode, SlotRunState, TaskStatus,
    UserBlockerAlert,
};
use agy_conductor::storage::store::AppStorage;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Notify};
use tokio::time::{timeout, Duration};

fn mk_slot(id: &str, alias: &str, role: RoleKind, priority: u8) -> AgentSlot {
    AgentSlot {
        slot_id: id.to_string(),
        alias: alias.to_string(),
        display_name: format!("{}-{alias}", role.label_zh()),
        model_id: format!("[Gateway] {alias}"),
        role,
        priority,
        enabled: true,
        state: SlotRunState::Idle,
        conversation_id: None,
        session_reused: false,
        current_activity: None,
        consecutive_failures: 0,
        usage: SlotUsageStats::default(),
    }
}

fn ok_resp(conv: &str, structured_json: serde_json::Value) -> AgentTurnResponse {
    AgentTurnResponse {
        conversation_id: Some(conv.to_string()),
        structured_json,
        raw_response: String::new(),
        duration_seconds: 0.2,
        tool_steps: 1,
        usage: TokenUsage::default(),
        peak_context_tokens: 0,
        switched_to_fresh_session: false,
    }
}

fn setup_temp_env(tag: &str) -> (PathBuf, PathBuf, RoleSchemaFiles) {
    let temp = std::env::temp_dir().join(format!(
        "agy-conductor-esc-{tag}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let storage = AppStorage::with_base_dir(temp.clone());
    storage.ensure_dirs().unwrap();
    let room_dir = storage.rooms_dir().join(format!("room-{tag}"));
    std::fs::create_dir_all(&room_dir).unwrap();
    let schemas = write_role_schemas(&storage.schemas_dir()).unwrap();
    (temp, room_dir, schemas)
}

/// Bug 1 回归：Coder B 仍在运行（`in_flight_turns > 0`）时，Coder A 上报 `BLOCKED_BY_ARCH`，
/// 空闲的总架构师必须立即被唤醒进行裁决，不能死锁等待 Coder B 结束。
struct BlockedWhileBusyRunner {
    t2_running: AtomicBool,
    arch_woke_while_t2_running: AtomicBool,
    t1_attempts: AtomicUsize,
    t2_release: Notify,
}

impl AgyRunner for BlockedWhileBusyRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _act_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role_file = req.schema_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if role_file.contains("architect") {
                if req.prompt.contains("全局终验门禁") {
                    return Ok(ok_resp("conv-arch", json!({
                        "action": "FINAL_ACCEPT",
                        "chat_message": "全部验收通过",
                        "architecture_notes": "done",
                        "tasks": [],
                        "verification_commands": ["cargo test"]
                    })));
                }
                if req.prompt.contains("架构自动优先裁决") {
                    if self.t2_running.load(Ordering::SeqCst) {
                        self.arch_woke_while_t2_running.store(true, Ordering::SeqCst);
                    }
                    self.t2_release.notify_waiters();
                    return Ok(ok_resp("conv-arch", json!({
                        "action": "DISPATCH_PLAN",
                        "chat_message": "已调整 T1 边界方案 @neon",
                        "architecture_notes": "已解决 T1 冲突",
                        "tasks": [{
                            "task_id": "T1",
                            "title": "模块 A（已放宽边界）",
                            "description": "按新契约实现 A",
                            "owned_paths": ["src/a/"],
                            "depends_on": [],
                            "preferred_coder_alias": "neon",
                            "acceptance_criteria": ["编译通过"]
                        }],
                        "verification_commands": ["cargo test"]
                    })));
                }
                return Ok(ok_resp("conv-arch", json!({
                    "action": "DISPATCH_PLAN",
                    "chat_message": "派发并行任务 T1 与 T2",
                    "architecture_notes": "初始契约",
                    "tasks": [
                        {
                            "task_id": "T1",
                            "title": "模块 A",
                            "description": "实现 A",
                            "owned_paths": ["src/a/"],
                            "depends_on": [],
                            "preferred_coder_alias": "neon",
                            "acceptance_criteria": ["ok"]
                        },
                        {
                            "task_id": "T2",
                            "title": "模块 B",
                            "description": "实现 B",
                            "owned_paths": ["src/b/"],
                            "depends_on": [],
                            "preferred_coder_alias": "hydrogen",
                            "acceptance_criteria": ["ok"]
                        }
                    ],
                    "verification_commands": ["cargo test"]
                })));
            }

            if role_file.contains("coder") {
                if req.prompt.contains("当前分配子任务：T2") {
                    self.t2_running.store(true, Ordering::SeqCst);
                    self.t2_release.notified().await;
                    self.t2_running.store(false, Ordering::SeqCst);
                    return Ok(ok_resp("conv-c2", json!({
                        "status": "SUBMIT_FOR_REVIEW",
                        "chat_message": "T2 完成",
                        "modified_files": ["src/b/mod.rs"],
                        "self_check_result": "ok",
                        "blocker_reason": null
                    })));
                }
                if self.t1_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(ok_resp("conv-c1", json!({
                        "status": "BLOCKED_BY_ARCH",
                        "chat_message": "T1 遇到接口冲突 @arch",
                        "modified_files": [],
                        "self_check_result": "blocked",
                        "blocker_reason": "T1 需要公共接口调整"
                    })));
                }
                return Ok(ok_resp("conv-c1", json!({
                    "status": "SUBMIT_FOR_REVIEW",
                    "chat_message": "T1 完成",
                    "modified_files": ["src/a/mod.rs"],
                    "self_check_result": "ok",
                    "blocker_reason": null
                })));
            }

            Ok(ok_resp("conv-rev", json!({
                "verdict": "APPROVED",
                "chat_message": "审查通过",
                "issues": [],
                "commands_verified": ["cargo test"]
            })))
        })
    }
}

#[tokio::test]
async fn blocked_task_wakes_idle_architect_while_other_coder_still_in_flight() {
    let (temp, room_dir, schemas) = setup_temp_env("bug1-blocked");
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (_ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(BlockedWhileBusyRunner {
        t2_running: AtomicBool::new(false),
        arch_woke_while_t2_running: AtomicBool::new(false),
        t1_attempts: AtomicUsize::new(0),
        t2_release: Notify::new(),
    });
    let engine = OrchestratorEngine::new(
        EngineConfig {
            room_id: "room-bug1".to_string(),
            room_dir,
            workspace_dir: temp.clone(),
            goal: "验证阻塞不死锁".to_string(),
            permission_mode: PermissionMode::AcceptEdits,
            enable_sandbox: false,
            max_rework_rounds: 3,
            turn_timeout_secs: 30,
            dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
            slots: vec![
                mk_slot("arch-1", "arch", RoleKind::Architect, 1),
                mk_slot("coder-1", "neon", RoleKind::Coder, 1),
                mk_slot("coder-2", "hydrogen", RoleKind::Coder, 2),
                mk_slot("rev-1", "rev", RoleKind::Reviewer, 1),
            ],
            schemas,
        },
        runner.clone(),
        evt_tx,
        ctrl_rx,
    );
    let handle = tokio::spawn(async move { engine.run().await });

    let mut blocker_alerts = 0usize;
    let mut completed = false;
    timeout(Duration::from_secs(8), async {
        while let Some(ev) = evt_rx.recv().await {
            match ev {
                EngineEvent::BlockerNeedsUser(_) => blocker_alerts += 1,
                EngineEvent::Completed(_) => {
                    completed = true;
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("总架构师必须在 T2 运行期间即刻醒来裁决，不能死锁超时");
    handle.await.unwrap();

    assert!(completed);
    assert!(runner.arch_woke_while_t2_running.load(Ordering::SeqCst));
    assert_eq!(blocker_alerts, 0, "第 1 次内部裁决不应弹窗打扰用户");
    let _ = std::fs::remove_dir_all(&temp);
}

/// Section 2 回归：第 1~2 次阻塞静默自动裁决，第 3 次触发 WaitingUser + 大白话弹窗，用户回复后恢复完成。
struct TwoTierEscalationRunner {
    coder_attempts: AtomicUsize,
    arch_interventions: AtomicUsize,
    user_decided: AtomicBool,
}

impl AgyRunner for TwoTierEscalationRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _act_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role_file = req.schema_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if role_file.contains("architect") {
                if req.prompt.contains("全局终验门禁") {
                    return Ok(ok_resp("conv-arch", json!({
                        "action": "FINAL_ACCEPT",
                        "chat_message": "用户拍板后全部验收通过",
                        "architecture_notes": "ok",
                        "tasks": [],
                        "verification_commands": ["cargo test"]
                    })));
                }
                if req.prompt.contains("回复 1：允许放宽改动范围") {
                    self.user_decided.store(true, Ordering::SeqCst);
                    return Ok(ok_resp("conv-arch", json!({
                        "action": "DISPATCH_PLAN",
                        "chat_message": "收到您拍板选择方案 1，已放宽范围交给程序员完成。",
                        "architecture_notes": "用户授权放宽范围",
                        "tasks": [{
                            "task_id": "T1",
                            "title": "导出中心（用户已拍板）",
                            "description": "按方案 1 统一调整",
                            "owned_paths": ["src/"],
                            "depends_on": [],
                            "preferred_coder_alias": null,
                            "acceptance_criteria": ["ok"]
                        }],
                        "verification_commands": ["cargo test"]
                    })));
                }
                if req.prompt.contains("架构自动优先裁决") {
                    self.arch_interventions.fetch_add(1, Ordering::SeqCst);
                    return Ok(ok_resp("conv-arch", json!({
                        "action": "DISPATCH_PLAN",
                        "chat_message": "总架构师尝试内部调整 T1 方案",
                        "architecture_notes": "内部自动裁决",
                        "tasks": [{
                            "task_id": "T1",
                            "title": "导出中心",
                            "description": "调整后重试",
                            "owned_paths": ["src/export/"],
                            "depends_on": [],
                            "preferred_coder_alias": null,
                            "acceptance_criteria": ["ok"]
                        }],
                        "verification_commands": ["cargo test"]
                    })));
                }
                return Ok(ok_resp("conv-arch", json!({
                    "action": "DISPATCH_PLAN",
                    "chat_message": "拆解 T1",
                    "architecture_notes": "init",
                    "tasks": [{
                        "task_id": "T1",
                        "title": "导出中心",
                        "description": "实现导出功能",
                        "owned_paths": ["src/export/"],
                        "depends_on": [],
                        "preferred_coder_alias": null,
                        "acceptance_criteria": ["ok"]
                    }],
                    "verification_commands": ["cargo test"]
                })));
            }

            if role_file.contains("coder") {
                let attempt = self.coder_attempts.fetch_add(1, Ordering::SeqCst);
                if attempt < 3 && !self.user_decided.load(Ordering::SeqCst) {
                    return Ok(ok_resp("conv-c1", json!({
                        "status": "BLOCKED_BY_ARCH",
                        "chat_message": format!("第 {} 次遇到改动边界冲突", attempt + 1),
                        "modified_files": [],
                        "self_check_result": "blocked",
                        "blocker_reason": "导出功能需要同时修改旧接口的外层调用"
                    })));
                }
                return Ok(ok_resp("conv-c1", json!({
                    "status": "SUBMIT_FOR_REVIEW",
                    "chat_message": "已按用户拍板完成导出中心",
                    "modified_files": ["src/export/mod.rs"],
                    "self_check_result": "ok",
                    "blocker_reason": null
                })));
            }

            Ok(ok_resp("conv-rev", json!({
                "verdict": "APPROVED",
                "chat_message": "审查通过",
                "issues": [],
                "commands_verified": ["cargo test"]
            })))
        })
    }
}

#[tokio::test]
async fn two_tier_escalation_arbitrates_twice_silently_then_alerts_user_and_resumes() {
    let (temp, room_dir, schemas) = setup_temp_env("two-tier");
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(TwoTierEscalationRunner {
        coder_attempts: AtomicUsize::new(0),
        arch_interventions: AtomicUsize::new(0),
        user_decided: AtomicBool::new(false),
    });

    let engine = OrchestratorEngine::new(
        EngineConfig {
            room_id: "room-twotier".to_string(),
            room_dir,
            workspace_dir: temp.clone(),
            goal: "实现导出中心".to_string(),
            permission_mode: PermissionMode::AcceptEdits,
            enable_sandbox: false,
            max_rework_rounds: 5,
            turn_timeout_secs: 30,
            dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
            slots: vec![
                mk_slot("arch-1", "arch", RoleKind::Architect, 1),
                mk_slot("coder-1", "neon", RoleKind::Coder, 1),
                mk_slot("rev-1", "rev", RoleKind::Reviewer, 1),
            ],
            schemas,
        },
        runner.clone(),
        evt_tx,
        ctrl_rx,
    );
    let handle = tokio::spawn(async move { engine.run().await });

    let mut received_alert: Option<UserBlockerAlert> = None;
    let mut saw_waiting_user = false;
    let mut completed = false;
    let mut final_tasks = Vec::new();

    timeout(Duration::from_secs(10), async {
        while let Some(ev) = evt_rx.recv().await {
            match ev {
                EngineEvent::PhaseChanged(RunPhase::WaitingUser) => saw_waiting_user = true,
                EngineEvent::BlockerNeedsUser(alert) => {
                    assert_eq!(
                        runner.arch_interventions.load(Ordering::SeqCst),
                        2,
                        "必须先由总架构师静默裁决 2 次后才弹窗提醒用户"
                    );
                    received_alert = Some(alert);
                    ctrl_tx
                        .send(EngineControl::PostUserMessage(
                            "回复 1：允许放宽改动范围".to_string(),
                        ))
                        .unwrap();
                }
                EngineEvent::TasksUpdated(t) => final_tasks = t,
                EngineEvent::Completed(_) => {
                    completed = true;
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("二级裁决闭环应在用户拍板后顺利完成");
    handle.await.unwrap();

    assert!(saw_waiting_user);
    assert!(completed);
    let alert = received_alert.expect("第 3 次阻塞应触发大白话用户拍板事件");
    assert_eq!(alert.reason_kind, "arbitration_limit_reached");
    assert!(alert.plain_explanation.contains("卡住的功能"));
    assert!(alert.plain_explanation.contains("回复 1"));
    assert!(final_tasks.iter().all(|t| t.status == TaskStatus::Approved));
    let _ = std::fs::remove_dir_all(&temp);
}
