use agy_conductor::agy::runner::{
    AgentActivityUpdate, AgentTurnError, AgentTurnRequest, AgentTurnResponse, AgyRunner, TurnFuture,
};
use agy_conductor::agy::stream_parser::{AgentErrorKind, TokenUsage};
use agy_conductor::billing::SlotUsageStats;
use agy_conductor::orchestrator::engine::EngineConfig;
use agy_conductor::orchestrator::schemas::{write_role_schemas, RoleSchemaFiles};
use agy_conductor::orchestrator::types::{
    AgentSlot, GroupChatMessage, MessageBadge, PermissionMode, RoleKind, SameRoleDispatchMode,
    SlotRunState, TaskItem, TaskStatus,
};
use agy_conductor::storage::store::AppStorage;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use tokio::sync::{mpsc, watch, Notify};

pub fn mk_slot(id: &str, alias: &str, role: RoleKind, priority: u8) -> AgentSlot {
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

pub fn ok_resp(conv: &str, structured_json: serde_json::Value) -> AgentTurnResponse {
    AgentTurnResponse {
        conversation_id: Some(conv.to_string()),
        structured_json,
        raw_response: String::new(),
        duration_seconds: 0.1,
        tool_steps: 1,
        usage: TokenUsage::default(),
        peak_context_tokens: 0,
        switched_to_fresh_session: false,
    }
}

pub fn mk_task(id: &str, deps: Vec<&str>, paths: Vec<&str>, status: TaskStatus) -> TaskItem {
    TaskItem {
        task_id: id.to_string(),
        title: format!("Task {id}"),
        description: format!("Desc {id}"),
        owned_paths: paths.into_iter().map(str::to_string).collect(),
        depends_on: deps.into_iter().map(str::to_string).collect(),
        preferred_coder_alias: None,
        acceptance_criteria: vec!["pass".to_string()],
        status,
        assigned_coder_slot_id: None,
        assigned_coder_label: None,
        last_coder_slot_id: None,
        rework_count: 0,
        arch_arbitration_count: 0,
        coder_failure_streak: 0,
        last_coder_summary: None,
        last_modified_files: Vec::new(),
        last_self_check: None,
        last_review_issues: Vec::new(),
        last_review_note: None,
        blocker_reason: None,
    }
}

pub fn setup_env(tag: &str) -> (PathBuf, PathBuf, RoleSchemaFiles) {
    let temp = std::env::temp_dir().join(format!(
        "agy-v6-rel-{tag}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let storage = AppStorage::with_base_dir(temp.clone());
    storage.ensure_dirs().unwrap();
    let room_dir = storage.rooms_dir().join(format!("room-{tag}"));
    std::fs::create_dir_all(&room_dir).unwrap();
    let schemas = write_role_schemas(&storage.schemas_dir()).unwrap();
    (temp, room_dir, schemas)
}

pub fn mk_engine_config(
    room_id: &str,
    room_dir: PathBuf,
    workspace_dir: PathBuf,
    goal: &str,
    slots: Vec<AgentSlot>,
    schemas: RoleSchemaFiles,
) -> EngineConfig {
    EngineConfig {
        room_id: room_id.to_string(),
        room_dir,
        workspace_dir,
        goal: goal.to_string(),
        permission_mode: PermissionMode::AcceptEdits,
        enable_sandbox: false,
        max_rework_rounds: 3,
        turn_timeout_secs: 30,
        dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
        slots,
        schemas,
    }
}

pub fn mk_external_cli_msg(id: &str, content: &str) -> GroupChatMessage {
    GroupChatMessage {
        id: id.to_string(),
        timestamp: "12:00:00".to_string(),
        sender_slot_id: Some("cli-coder".to_string()),
        sender_alias: "cli".to_string(),
        sender_display: "外部 CLI".to_string(),
        sender_role: Some(RoleKind::Coder),
        priority: Some(1),
        is_user: false,
        is_system: false,
        content: content.to_string(),
        badge: MessageBadge::Info,
        task_id: None,
        modified_files: Vec::new(),
        duration_seconds: None,
        tool_steps: None,
        issues: Vec::new(),
    }
}

pub struct SimplePlanRunner {
    pub arch_calls: AtomicUsize,
}

impl AgyRunner for SimplePlanRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _act_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role = req.schema_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if role.contains("architect") {
                self.arch_calls.fetch_add(1, Ordering::SeqCst);
                if req.prompt.contains("全局终验门禁") {
                    return Ok(ok_resp("c-a", json!({
                        "action": "FINAL_ACCEPT",
                        "chat_message": "终验通过",
                        "architecture_notes": "ok",
                        "tasks": [],
                        "verification_commands": []
                    })));
                }
                return Ok(ok_resp("c-a", json!({
                    "action": "DISPATCH_PLAN",
                    "chat_message": "拆单 T1",
                    "architecture_notes": "ok",
                    "tasks": [{
                        "task_id": "T1", "title": "Init", "description": "d",
                        "owned_paths": ["src/a.rs"], "depends_on": [],
                        "preferred_coder_alias": null, "acceptance_criteria": ["ok"]
                    }],
                    "verification_commands": []
                })));
            }
            if role.contains("coder") {
                return Ok(ok_resp("c-c", json!({
                    "status": "SUBMIT_FOR_REVIEW", "chat_message": "done",
                    "modified_files": ["src/a.rs"], "self_check_result": "ok", "blocker_reason": null
                })));
            }
            Ok(ok_resp("c-r", json!({
                "verdict": "APPROVED", "chat_message": "ok", "issues": [], "commands_verified": []
            })))
        })
    }
}

pub struct InterjectionSwapRunner {
    pub arch1_started: Notify,
    pub arch1_canceled: AtomicBool,
    pub arch2_seen_prompts: Mutex<Vec<String>>,
}

impl AgyRunner for InterjectionSwapRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _act_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        mut cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role = req.schema_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if role.contains("architect") {
                if req.slot_id == "arch-1" && req.prompt.contains("热切换不能丢失的指令") {
                    self.arch1_started.notify_waiters();
                    loop {
                        if *cancel_rx.borrow() {
                            self.arch1_canceled.store(true, Ordering::SeqCst);
                            return Err(AgentTurnError {
                                kind: AgentErrorKind::Canceled,
                                message: "swapped".to_string(),
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
                if req.slot_id == "arch-2" {
                    self.arch2_seen_prompts.lock().unwrap().push(req.prompt.clone());
                    return Ok(ok_resp("c-a2", json!({
                        "action": "FINAL_ACCEPT",
                        "chat_message": "已收到插话并完成",
                        "architecture_notes": "ok",
                        "tasks": [],
                        "verification_commands": []
                    })));
                }
            }
            Ok(ok_resp("c-other", json!({
                "action": "FINAL_ACCEPT", "chat_message": "ok", "architecture_notes": "ok",
                "tasks": [], "verification_commands": []
            })))
        })
    }
}

pub struct ContractChangeRunner {
    pub coder_round: AtomicUsize,
    pub first_coder_started: Notify,
    pub first_coder_canceled: AtomicBool,
    pub second_coder_saw_new_contract: AtomicBool,
}

impl AgyRunner for ContractChangeRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _act_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        mut cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role = req.schema_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if role.contains("architect") {
                if req.prompt.contains("全局终验门禁") {
                    return Ok(ok_resp("a", json!({
                        "action": "FINAL_ACCEPT", "chat_message": "ok",
                        "architecture_notes": "ok", "tasks": [], "verification_commands": []
                    })));
                }
                if req.prompt.contains("请把 T1 改为输出 JSON") {
                    return Ok(ok_resp("a", json!({
                        "action": "DISPATCH_PLAN", "chat_message": "更新 T1 契约为 JSON",
                        "architecture_notes": "v2",
                        "tasks": [{
                            "task_id": "T1", "title": "导出器", "description": "必须输出严格 JSON 格式",
                            "owned_paths": ["src/exp.rs"], "depends_on": [],
                            "preferred_coder_alias": null, "acceptance_criteria": ["支持 JSON 导出"]
                        }],
                        "verification_commands": []
                    })));
                }
                return Ok(ok_resp("a", json!({
                    "action": "DISPATCH_PLAN", "chat_message": "初始拆单 T1",
                    "architecture_notes": "v1",
                    "tasks": [{
                        "task_id": "T1", "title": "导出器", "description": "输出 CSV 格式",
                        "owned_paths": ["src/exp.rs"], "depends_on": [],
                        "preferred_coder_alias": null, "acceptance_criteria": ["支持 CSV 导出"]
                    }],
                    "verification_commands": []
                })));
            }
            if role.contains("coder") {
                let n = self.coder_round.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    self.first_coder_started.notify_waiters();
                    loop {
                        if *cancel_rx.borrow() {
                            self.first_coder_canceled.store(true, Ordering::SeqCst);
                            return Err(AgentTurnError {
                                kind: AgentErrorKind::Canceled,
                                message: "contract invalidated".to_string(),
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
                if req.prompt.contains("必须输出严格 JSON 格式") {
                    self.second_coder_saw_new_contract.store(true, Ordering::SeqCst);
                }
                return Ok(ok_resp("c", json!({
                    "status": "SUBMIT_FOR_REVIEW", "chat_message": "完成新契约",
                    "modified_files": ["src/exp.rs"], "self_check_result": "ok", "blocker_reason": null
                })));
            }
            Ok(ok_resp("r", json!({
                "verdict": "APPROVED", "chat_message": "ok", "issues": [], "commands_verified": []
            })))
        })
    }
}

pub struct CyclicThenFixedDagRunner {
    pub plan_attempts: AtomicUsize,
}

impl AgyRunner for CyclicThenFixedDagRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _act_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role = req.schema_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if role.contains("architect") {
                if req.prompt.contains("全局终验门禁") {
                    return Ok(ok_resp("a", json!({
                        "action": "FINAL_ACCEPT", "chat_message": "ok",
                        "architecture_notes": "ok", "tasks": [], "verification_commands": []
                    })));
                }
                let attempt = self.plan_attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    return Ok(ok_resp("a", json!({
                        "action": "DISPATCH_PLAN", "chat_message": "带环规划",
                        "architecture_notes": "bad",
                        "tasks": [
                            { "task_id": "T1", "title": "A", "description": "A", "owned_paths": ["src/a.rs"], "depends_on": ["T2"], "preferred_coder_alias": null, "acceptance_criteria": ["ok"] },
                            { "task_id": "T2", "title": "B", "description": "B", "owned_paths": ["src/b.rs"], "depends_on": ["T1"], "preferred_coder_alias": null, "acceptance_criteria": ["ok"] }
                        ],
                        "verification_commands": []
                    })));
                }
                return Ok(ok_resp("a", json!({
                    "action": "DISPATCH_PLAN", "chat_message": "已修正为无环 DAG",
                    "architecture_notes": "good",
                    "tasks": [
                        { "task_id": "T1", "title": "A", "description": "A", "owned_paths": ["src/a.rs"], "depends_on": [], "preferred_coder_alias": null, "acceptance_criteria": ["ok"] },
                        { "task_id": "T2", "title": "B", "description": "B", "owned_paths": ["src/b.rs"], "depends_on": ["T1"], "preferred_coder_alias": null, "acceptance_criteria": ["ok"] }
                    ],
                    "verification_commands": []
                })));
            }
            if role.contains("coder") {
                return Ok(ok_resp("c", json!({
                    "status": "SUBMIT_FOR_REVIEW", "chat_message": "ok",
                    "modified_files": ["src/a.rs"], "self_check_result": "ok", "blocker_reason": null
                })));
            }
            Ok(ok_resp("r", json!({
                "verdict": "APPROVED", "chat_message": "ok", "issues": [], "commands_verified": []
            })))
        })
    }
}
