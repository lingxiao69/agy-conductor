use agy_conductor::agy::runner::{
    AgentActivityUpdate, AgentTurnError, AgentTurnRequest, AgentTurnResponse, AgyRunner,
    TurnFuture,
};
use agy_conductor::agy::stream_parser::{AgentErrorKind, TokenUsage};
use agy_conductor::billing::SlotUsageStats;
use agy_conductor::orchestrator::engine::{
    EngineConfig, EngineControl, EngineEvent, OrchestratorEngine,
};
use agy_conductor::orchestrator::schemas::{write_role_schemas, RoleSchemaFiles};
use agy_conductor::orchestrator::types::{
    AgentSlot, PermissionMode, RoleKind, RunPhase, SameRoleDispatchMode, SlotRunState,
    UserBlockerAlert,
};
use agy_conductor::storage::store::AppStorage;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
        "agy-conductor-ghost-{tag}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let storage = AppStorage::with_base_dir(temp.clone());
    storage.ensure_dirs().unwrap();
    let room_dir = storage.rooms_dir().join(format!("room-{tag}"));
    std::fs::create_dir_all(&room_dir).unwrap();
    let schemas = write_role_schemas(&storage.schemas_dir()).unwrap();
    (temp, room_dir, schemas)
}

/// Bug 2 回归：运行中途删除慢槽位时，立即取消该槽位后台轮次并回收任务给健康槽位接力完成。
struct GhostCancelRunner {
    slow_started: Notify,
    slow_was_canceled: AtomicBool,
    backup_ran_t1: AtomicBool,
}

impl AgyRunner for GhostCancelRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _act_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        mut cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role_file = req.schema_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if role_file.contains("architect") {
                if req.prompt.contains("全局终验门禁") {
                    return Ok(ok_resp("conv-arch", json!({
                        "action": "FINAL_ACCEPT",
                        "chat_message": "终验通过",
                        "architecture_notes": "ok",
                        "tasks": [],
                        "verification_commands": ["cargo test"]
                    })));
                }
                return Ok(ok_resp("conv-arch", json!({
                    "action": "DISPATCH_PLAN",
                    "chat_message": "派发 T1",
                    "architecture_notes": "ok",
                    "tasks": [{
                        "task_id": "T1",
                        "title": "核心模块",
                        "description": "实现核心逻辑",
                        "owned_paths": ["src/"],
                        "depends_on": [],
                        "preferred_coder_alias": null,
                        "acceptance_criteria": ["ok"]
                    }],
                    "verification_commands": ["cargo test"]
                })));
            }

            if role_file.contains("coder") {
                if req.slot_id == "coder-slow" {
                    self.slow_started.notify_waiters();
                    loop {
                        if *cancel_rx.borrow() {
                            self.slow_was_canceled.store(true, Ordering::SeqCst);
                            return Err(AgentTurnError {
                                kind: AgentErrorKind::Canceled,
                                message: "slot removed".to_string(),
                                conversation_id: None,
                                partial_usage: None,
                                peak_context_tokens: 0,
                            });
                        }
                        if cancel_rx.changed().await.is_err() {
                            break;
                        }
                    }
                }
                if req.slot_id == "coder-backup" {
                    self.backup_ran_t1.store(true, Ordering::SeqCst);
                }
                return Ok(ok_resp("conv-backup", json!({
                    "status": "SUBMIT_FOR_REVIEW",
                    "chat_message": "健康槽位接力完成 T1",
                    "modified_files": ["src/lib.rs"],
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
async fn removing_active_slot_cancels_ghost_process_and_recovers_task_to_healthy_slot() {
    let (temp, room_dir, schemas) = setup_temp_env("bug2");
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(GhostCancelRunner {
        slow_started: Notify::new(),
        slow_was_canceled: AtomicBool::new(false),
        backup_ran_t1: AtomicBool::new(false),
    });

    let arch = mk_slot("arch-1", "arch", RoleKind::Architect, 1);
    let slow = mk_slot("coder-slow", "slow", RoleKind::Coder, 1);
    let backup = mk_slot("coder-backup", "backup", RoleKind::Coder, 2);
    let rev = mk_slot("rev-1", "rev", RoleKind::Reviewer, 1);

    let engine = OrchestratorEngine::new(
        EngineConfig {
            room_id: "room-bug2".to_string(),
            room_dir,
            workspace_dir: temp.clone(),
            goal: "验证移除槽位取消幽灵进程".to_string(),
            permission_mode: PermissionMode::AcceptEdits,
            enable_sandbox: false,
            max_rework_rounds: 3,
            turn_timeout_secs: 30,
            dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
            slots: vec![arch.clone(), slow, backup.clone(), rev.clone()],
            schemas,
        },
        runner.clone(),
        evt_tx,
        ctrl_rx,
    );
    let handle = tokio::spawn(async move { engine.run().await });

    timeout(Duration::from_secs(5), runner.slow_started.notified())
        .await
        .expect("coder-slow 应启动 T1");
    ctrl_tx
        .send(EngineControl::UpdateSlots(vec![arch, backup, rev]))
        .unwrap();

    let mut completed = false;
    timeout(Duration::from_secs(8), async {
        while let Some(ev) = evt_rx.recv().await {
            if let EngineEvent::Completed(_) = ev {
                completed = true;
                break;
            }
        }
    })
    .await
    .expect("移除慢槽位后健康槽位必须立即接力完成");
    handle.await.unwrap();

    assert!(completed);
    assert!(runner.slow_was_canceled.load(Ordering::SeqCst));
    assert!(runner.backup_ran_t1.load(Ordering::SeqCst));
    let _ = std::fs::remove_dir_all(&temp);
}

/// Bug 1 + Section 2.2 回归：
/// 1) 程序员运行中（`in_flight_turns > 0`），用户发消息会立即唤醒空闲总架构师（不死锁）；
/// 2) 总架构师主动返回 `NEED_USER_CLARIFICATION` 时，触发 `WaitingUser` + `BlockerNeedsUser` 大白话提醒；
/// 3) 总架构师忙碌期间用户再发消息，立刻收到系统即时回显确认。
struct ClarificationAndInterjectionRunner {
    coder_running: AtomicBool,
    coder_started: Notify,
    coder_release: Notify,
    arch_woke_while_coder_running: AtomicBool,
    arch_clarify_started: Notify,
    arch_clarify_release: Notify,
    asked_user_once: AtomicBool,
}

impl AgyRunner for ClarificationAndInterjectionRunner {
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
                        "chat_message": "验收通过",
                        "architecture_notes": "done",
                        "tasks": [],
                        "verification_commands": []
                    })));
                }
                if req.prompt.contains("回复 1：用本地账号密码登录") {
                    return Ok(ok_resp("conv-arch", json!({
                        "action": "DISPATCH_PLAN",
                        "chat_message": "明白，采用方案 1（本地账号密码登录），团队继续推进。",
                        "architecture_notes": "方案 1",
                        "tasks": [],
                        "verification_commands": []
                    })));
                }
                if req.prompt.contains("中途加个登录方式确认") {
                    if self.coder_running.load(Ordering::SeqCst) {
                        self.arch_woke_while_coder_running.store(true, Ordering::SeqCst);
                    }
                    // 等待测试向忙碌的总架构师发送第二条插话以验证即时回显
                    self.arch_clarify_started.notify_waiters();
                    self.arch_clarify_release.notified().await;
                    self.coder_release.notify_waiters();
                    if !self.asked_user_once.swap(true, Ordering::SeqCst) {
                        return Ok(ok_resp("conv-arch", json!({
                            "action": "NEED_USER_CLARIFICATION",
                            "chat_message": "登录方式存在业务分歧，请确认。",
                            "user_plain_explanation": "【卡住的功能】登录方式还没定。\n回复 1：用本地账号密码登录；\n回复 2：用企业扫码登录。",
                            "architecture_notes": "待确认登录方式",
                            "tasks": [],
                            "verification_commands": []
                        })));
                    }
                }
                return Ok(ok_resp("conv-arch", json!({
                    "action": "DISPATCH_PLAN",
                    "chat_message": "派发 T1",
                    "architecture_notes": "init",
                    "tasks": [{
                        "task_id": "T1",
                        "title": "基础页面",
                        "description": "搭建基础页面",
                        "owned_paths": ["src/ui/"],
                        "depends_on": [],
                        "preferred_coder_alias": null,
                        "acceptance_criteria": ["ok"]
                    }],
                    "verification_commands": []
                })));
            }

            if role_file.contains("coder") {
                self.coder_running.store(true, Ordering::SeqCst);
                self.coder_started.notify_waiters();
                self.coder_release.notified().await;
                self.coder_running.store(false, Ordering::SeqCst);
                return Ok(ok_resp("conv-c1", json!({
                    "status": "SUBMIT_FOR_REVIEW",
                    "chat_message": "T1 完成",
                    "modified_files": ["src/ui/mod.rs"],
                    "self_check_result": "ok",
                    "blocker_reason": null
                })));
            }

            Ok(ok_resp("conv-rev", json!({
                "verdict": "APPROVED",
                "chat_message": "审查通过",
                "issues": [],
                "commands_verified": []
            })))
        })
    }
}

#[tokio::test]
async fn user_interjection_wakes_architect_immediately_and_clarification_emits_plain_alert() {
    let (temp, room_dir, schemas) = setup_temp_env("clarify-interject");
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(ClarificationAndInterjectionRunner {
        coder_running: AtomicBool::new(false),
        coder_started: Notify::new(),
        coder_release: Notify::new(),
        arch_woke_while_coder_running: AtomicBool::new(false),
        arch_clarify_started: Notify::new(),
        arch_clarify_release: Notify::new(),
        asked_user_once: AtomicBool::new(false),
    });

    let engine = OrchestratorEngine::new(
        EngineConfig {
            room_id: "room-clarify".to_string(),
            room_dir,
            workspace_dir: temp.clone(),
            goal: "验证插话与大白话澄清弹窗".to_string(),
            permission_mode: PermissionMode::AcceptEdits,
            enable_sandbox: false,
            max_rework_rounds: 3,
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

    // 1. 等 Coder 正在执行时发用户消息：空闲的总架构师应立即醒来，无需等 Coder 结束
    timeout(Duration::from_secs(5), runner.coder_started.notified())
        .await
        .expect("coder 应启动 T1");
    ctrl_tx
        .send(EngineControl::PostUserMessage("中途加个登录方式确认".to_string()))
        .unwrap();

    // 2. 等总架构师进入忙碌态后，再发一条插话，应立刻收到系统回显提示
    timeout(Duration::from_secs(5), runner.arch_clarify_started.notified())
        .await
        .expect("总架构师应在 Coder 运行期间立即醒来响应用户");
    ctrl_tx
        .send(EngineControl::PostUserMessage("补充一条提示".to_string()))
        .unwrap();
    runner.arch_clarify_release.notify_waiters();

    let mut saw_busy_ack = false;
    let mut alert_opt: Option<UserBlockerAlert> = None;
    let mut saw_waiting = false;
    let mut completed = false;

    timeout(Duration::from_secs(10), async {
        while let Some(ev) = evt_rx.recv().await {
            match ev {
                EngineEvent::ChatMessageAppended(msg) => {
                    if msg.is_system && msg.content.contains("总架构师当前轮次收敛后将第一时间优先响应") {
                        saw_busy_ack = true;
                    }
                }
                EngineEvent::PhaseChanged(RunPhase::WaitingUser) => saw_waiting = true,
                EngineEvent::BlockerNeedsUser(alert) => {
                    alert_opt = Some(alert);
                    ctrl_tx
                        .send(EngineControl::PostUserMessage(
                            "回复 1：用本地账号密码登录".to_string(),
                        ))
                        .unwrap();
                }
                EngineEvent::Completed(_) => {
                    completed = true;
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("用户拍板后应恢复完成");
    handle.await.unwrap();

    assert!(runner.arch_woke_while_coder_running.load(Ordering::SeqCst));
    assert!(saw_busy_ack, "总架构师忙碌时发消息应立即回显系统确认");
    assert!(saw_waiting);
    let alert = alert_opt.expect("NEED_USER_CLARIFICATION 必须触发大白话拍板事件");
    assert_eq!(alert.reason_kind, "need_user_clarification");
    assert!(alert.plain_explanation.contains("登录方式还没定"));
    assert!(alert.plain_explanation.contains("回复 1"));
    assert!(completed);
    let _ = std::fs::remove_dir_all(&temp);
}
