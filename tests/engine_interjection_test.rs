use agy_conductor::agy::runner::{
    AgentActivityUpdate, AgentTurnError, AgentTurnRequest, AgentTurnResponse, AgyRunner,
    TurnFuture,
};
use agy_conductor::agy::stream_parser::{AgentErrorKind, TokenUsage};
use agy_conductor::agy::updater::validate_can_update;
use agy_conductor::billing::SlotUsageStats;
use agy_conductor::orchestrator::engine::{
    EngineConfig, EngineControl, EngineEvent, OrchestratorEngine,
};
use agy_conductor::orchestrator::prompt_builder::build_architect_planning_prompt;
use agy_conductor::orchestrator::schemas::write_role_schemas;
use agy_conductor::orchestrator::types::{
    AgentSlot, PermissionMode, RoleKind, RunPhase, SameRoleDispatchMode, SlotRunState, TaskStatus,
};
use agy_conductor::storage::store::AppStorage;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};
use tokio::time::{timeout, Duration};

fn make_slots() -> Vec<AgentSlot> {
    vec![
        AgentSlot {
            slot_id: "arch-1".to_string(),
            alias: "carbon-arch".to_string(),
            display_name: "总架构师 · Carbon".to_string(),
            model_id: "[Gateway] gemini-4-carbon-high".to_string(),
            role: RoleKind::Architect,
            priority: 1,
            enabled: true,
            state: SlotRunState::Idle,
            conversation_id: None,
            session_reused: false,
            current_activity: None,
            consecutive_failures: 0,
            usage: SlotUsageStats::default(),
        },
        AgentSlot {
            slot_id: "coder-1".to_string(),
            alias: "neon".to_string(),
            display_name: "程序员 · Neon".to_string(),
            model_id: "[Gateway] gemini-4-neon-xhigh".to_string(),
            role: RoleKind::Coder,
            priority: 1,
            enabled: true,
            state: SlotRunState::Idle,
            conversation_id: None,
            session_reused: false,
            current_activity: None,
            consecutive_failures: 0,
            usage: SlotUsageStats::default(),
        },
        AgentSlot {
            slot_id: "rev-1".to_string(),
            alias: "carbon-rev".to_string(),
            display_name: "审查员 · Carbon".to_string(),
            model_id: "[Gateway] gemini-4-carbon-high".to_string(),
            role: RoleKind::Reviewer,
            priority: 1,
            enabled: true,
            state: SlotRunState::Idle,
            conversation_id: None,
            session_reused: false,
            current_activity: None,
            consecutive_failures: 0,
            usage: SlotUsageStats::default(),
        },
    ]
}

#[derive(Default)]
struct InterjectionMockState {
    saw_user_interjection: bool,
    extra_task_dispatched: bool,
    completed_tasks: Vec<String>,
}

struct InterjectionMockRunner {
    state: Mutex<InterjectionMockState>,
    control_tx: mpsc::UnboundedSender<EngineControl>,
}

impl AgyRunner for InterjectionMockRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _activity_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role_file = req
                .schema_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");

            if role_file.contains("architect") {
                if req.prompt.contains("额外补充 README 验收说明") {
                    let mut st = self.state.lock().unwrap();
                    st.saw_user_interjection = true;
                    st.extra_task_dispatched = true;
                    return Ok(AgentTurnResponse {
                        conversation_id: Some("conv-arch-1".to_string()),
                        structured_json: json!({
                            "action": "DISPATCH_PLAN",
                            "chat_message": "收到用户中途插话，追加 T2 文档验收任务 @neon。",
                            "architecture_notes": "补充文档验收",
                            "tasks": [{
                                "task_id": "T2",
                                "title": "补充 README 验收说明",
                                "description": "完善 README 验收章节",
                                "owned_paths": ["docs/"],
                                "depends_on": ["T1"],
                                "preferred_coder_alias": null,
                                "acceptance_criteria": ["包含完整验收步骤"]
                            }],
                            "verification_commands": ["cargo test"]
                        }),
                        raw_response: String::new(),
                        duration_seconds: 0.4,
                        tool_steps: 1,
                        usage: TokenUsage::default(),
                        peak_context_tokens: 0,
                        switched_to_fresh_session: false,
                    });
                }

                if req.prompt.contains("全局终验门禁") {
                    return Ok(AgentTurnResponse {
                        conversation_id: Some("conv-arch-1".to_string()),
                        structured_json: json!({
                            "action": "FINAL_ACCEPT",
                            "chat_message": "T1 与追加的 T2 均已验收通过，正式交付。",
                            "architecture_notes": "done",
                            "tasks": [],
                            "verification_commands": ["cargo test"]
                        }),
                        raw_response: String::new(),
                        duration_seconds: 0.4,
                        tool_steps: 1,
                        usage: TokenUsage::default(),
                        peak_context_tokens: 0,
                        switched_to_fresh_session: false,
                    });
                }

                return Ok(AgentTurnResponse {
                    conversation_id: Some("conv-arch-1".to_string()),
                    structured_json: json!({
                        "action": "DISPATCH_PLAN",
                        "chat_message": "已拆解初始任务 T1 @neon。",
                        "architecture_notes": "模块化设计",
                        "tasks": [{
                            "task_id": "T1",
                            "title": "实现核心模块",
                            "description": "编写核心逻辑",
                            "owned_paths": ["src/"],
                            "depends_on": [],
                            "preferred_coder_alias": null,
                            "acceptance_criteria": ["编译通过"]
                        }],
                        "verification_commands": ["cargo test"]
                    }),
                    raw_response: String::new(),
                    duration_seconds: 0.4,
                    tool_steps: 1,
                    usage: TokenUsage::default(),
                    peak_context_tokens: 0,
                    switched_to_fresh_session: false,
                });
            }

            if role_file.contains("coder") {
                let task_id = if req.prompt.contains("T2") { "T2" } else { "T1" };
                if task_id == "T1" {
                    let _ = self.control_tx.send(EngineControl::PostUserMessage(
                        "请额外补充 README 验收说明".to_string(),
                    ));
                }
                self.state
                    .lock()
                    .unwrap()
                    .completed_tasks
                    .push(task_id.to_string());
                return Ok(AgentTurnResponse {
                    conversation_id: Some(format!("conv-coder-{task_id}")),
                    structured_json: json!({
                        "status": "SUBMIT_FOR_REVIEW",
                        "chat_message": format!("{task_id} 编码完成 @carbon-rev 请审查。"),
                        "modified_files": [format!("src/{task_id}.rs")],
                        "self_check_result": "passed",
                        "blocker_reason": null
                    }),
                    raw_response: String::new(),
                    duration_seconds: 0.5,
                    tool_steps: 2,
                    usage: TokenUsage::default(),
                    peak_context_tokens: 0,
                    switched_to_fresh_session: false,
                });
            }

            if role_file.contains("reviewer") {
                return Ok(AgentTurnResponse {
                    conversation_id: Some("conv-rev-1".to_string()),
                    structured_json: json!({
                        "verdict": "APPROVED",
                        "chat_message": "审查通过。",
                        "issues": [],
                        "commands_verified": ["cargo test"]
                    }),
                    raw_response: String::new(),
                    duration_seconds: 0.3,
                    tool_steps: 1,
                    usage: TokenUsage::default(),
                    peak_context_tokens: 0,
                    switched_to_fresh_session: false,
                });
            }

            Err(AgentTurnError {
                kind: AgentErrorKind::ProcessFailed,
                message: "unexpected role".to_string(),
                conversation_id: None,
                partial_usage: None,
                peak_context_tokens: 0,
            })
        })
    }
}

#[tokio::test]
async fn mid_run_user_interjection_dispatches_additional_task_before_final_accept() {
    let temp = std::env::temp_dir().join(format!(
        "agy-conductor-interjection-{}",
        uuid::Uuid::new_v4()
    ));
    let storage = AppStorage::with_base_dir(temp.clone());
    storage.ensure_dirs().unwrap();
    let room_dir = storage.rooms_dir().join("room-interject");
    std::fs::create_dir_all(&room_dir).unwrap();
    let schemas = write_role_schemas(&storage.schemas_dir()).unwrap();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(InterjectionMockRunner {
        state: Mutex::new(InterjectionMockState::default()),
        control_tx: control_tx.clone(),
    });

    let engine = OrchestratorEngine::new(
        EngineConfig {
            room_id: "room-interject".to_string(),
            room_dir,
            workspace_dir: temp.clone(),
            goal: "实现核心服务".to_string(),
            permission_mode: PermissionMode::AcceptEdits,
            enable_sandbox: false,
            max_rework_rounds: 5,
            turn_timeout_secs: 30,
            dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
            slots: make_slots(),
            schemas,
        },
        runner.clone(),
        event_tx,
        control_rx,
    );

    let handle = tokio::spawn(async move {
        engine.run().await;
    });

    let mut final_tasks = Vec::new();
    let mut completed = None;
    timeout(Duration::from_secs(10), async {
        while let Some(ev) = event_rx.recv().await {
            match ev {
                EngineEvent::TasksUpdated(t) => final_tasks = t,
                EngineEvent::Completed(summary) => {
                    completed = Some(summary);
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("interjection test should complete within timeout");

    handle.await.unwrap();

    let st = runner.state.lock().unwrap();
    assert!(
        st.saw_user_interjection,
        "Architect should receive user interjection"
    );
    assert!(st.extra_task_dispatched, "Architect should append T2");
    assert_eq!(final_tasks.len(), 2);
    assert!(final_tasks.iter().all(|t| t.status == TaskStatus::Approved));
    let summary = completed.expect("should reach Completed");
    assert_eq!(summary.total_tasks, 2);

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn update_precheck_blocks_busy_or_attached_cli_slots() {
    let mut slots = make_slots();
    assert!(validate_can_update(&slots).is_ok());

    slots[1].state = SlotRunState::Working;
    let err = validate_can_update(&slots).unwrap_err();
    assert!(err.contains("活跃") || err.contains("占用"));

    slots[1].state = SlotRunState::ExternalCliAttached;
    let err2 = validate_can_update(&slots).unwrap_err();
    assert!(err2.contains("程序员 · Neon"));

    slots[1].state = SlotRunState::Paused;
    assert!(validate_can_update(&slots).is_ok());
}

struct ArchitectOnlyReviewRunner {
    non_arch_calls: Mutex<u32>,
}

impl AgyRunner for ArchitectOnlyReviewRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _activity_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let role_file = req
                .schema_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if !role_file.contains("architect") {
                *self.non_arch_calls.lock().unwrap() += 1;
                return Err(AgentTurnError {
                    kind: AgentErrorKind::ProcessFailed,
                    message: "should not dispatch coder or reviewer".to_string(),
                    conversation_id: None,
                    partial_usage: None,
                    peak_context_tokens: 0,
                });
            }
            Ok(AgentTurnResponse {
                conversation_id: Some("conv-arch-readonly".to_string()),
                structured_json: json!({
                    "action": "DISPATCH_PLAN",
                    "chat_message": "只读架构审查结论：全仓模块边界清晰，无需改代码。",
                    "architecture_notes": "只读审查通过",
                    "tasks": [],
                    "verification_commands": []
                }),
                raw_response: String::new(),
                duration_seconds: 0.3,
                tool_steps: 2,
                usage: TokenUsage::default(),
                peak_context_tokens: 0,
                switched_to_fresh_session: false,
            })
        })
    }
}

#[tokio::test]
async fn architect_zero_subtasks_dispatch_plan_completes_without_idle_spin() {
    let slots = make_slots();
    let prompt = build_architect_planning_prompt(
        "仅做只读架构审查并给出结论",
        "D:\\work\\demo",
        &slots,
        &[],
        &[],
        None,
    );
    assert!(prompt.contains("FINAL_ACCEPT"));
    assert!(prompt.contains("tasks: []"));

    let temp = std::env::temp_dir().join(format!(
        "agy-conductor-zero-task-{}",
        uuid::Uuid::new_v4()
    ));
    let storage = AppStorage::with_base_dir(temp.clone());
    storage.ensure_dirs().unwrap();
    let room_dir = storage.rooms_dir().join("room-zero-task");
    std::fs::create_dir_all(&room_dir).unwrap();
    let schemas = write_role_schemas(&storage.schemas_dir()).unwrap();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (_control_tx, control_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(ArchitectOnlyReviewRunner {
        non_arch_calls: Mutex::new(0),
    });

    let engine = OrchestratorEngine::new(
        EngineConfig {
            room_id: "room-zero-task".to_string(),
            room_dir,
            workspace_dir: temp.clone(),
            goal: "仅做只读架构审查并给出结论".to_string(),
            permission_mode: PermissionMode::AcceptEdits,
            enable_sandbox: false,
            max_rework_rounds: 3,
            turn_timeout_secs: 30,
            dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
            slots,
            schemas,
        },
        runner.clone(),
        event_tx,
        control_rx,
    );

    let handle = tokio::spawn(async move { engine.run().await });

    let mut phases = Vec::new();
    let mut completed = None;
    timeout(Duration::from_secs(5), async {
        while let Some(ev) = event_rx.recv().await {
            match ev {
                EngineEvent::PhaseChanged(p) => phases.push(p),
                EngineEvent::Completed(summary) => {
                    completed = Some(summary);
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("zero-subtask architect conclusion must complete immediately without spinning");

    handle.await.unwrap();

    let summary = completed.expect("must emit Completed summary");
    assert_eq!(summary.total_tasks, 0);
    assert!(summary
        .architect_note
        .contains("只读架构审查结论：全仓模块边界清晰，无需改代码。"));
    assert!(phases.contains(&RunPhase::Completed));
    assert!(!phases.contains(&RunPhase::Running));
    assert_eq!(*runner.non_arch_calls.lock().unwrap(), 0);

    let _ = std::fs::remove_dir_all(&temp);
}
