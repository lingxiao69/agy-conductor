use agy_conductor::agy::live_log::{log_file_for_slot, LiveLogSink, LiveStreamKind};
use agy_conductor::agy::resolver::{AgyLaunchStrategy, ResolvedAgyRuntime};
use agy_conductor::agy::runner::{
    AgentActivityUpdate, AgentTurnRequest, AgentTurnResponse, AgyRunner, TurnFuture,
};
use agy_conductor::agy::stream_parser::{
    extract_live_stream_lines, is_quota_error_text, TokenUsage,
};
use agy_conductor::agy::terminal::{
    build_wt_args_for_wrappers, escape_wt_arg, prepare_terminal_wrapper, TerminalLaunchOptions,
};
use agy_conductor::billing::SlotUsageStats;
use agy_conductor::ipc::session_terminal_actions::apply_session_reuse_for_slots;
use agy_conductor::orchestrator::engine::{EngineConfig, EngineEvent, OrchestratorEngine};
use agy_conductor::orchestrator::prompt_builder::build_architect_planning_prompt;
use agy_conductor::orchestrator::schemas::write_role_schemas;
use agy_conductor::orchestrator::types::{
    AgentSlot, PermissionMode, RoleKind, SameRoleDispatchMode, SlotRunState,
};
use agy_conductor::storage::store::AppStorage;
use agy_conductor::storage::workspace_sessions::{
    discover_workspace_role_sessions, upsert_workspace_role_sessions, WorkspaceRoleSession,
    SESSION_SOURCE_CONDUCTOR,
};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};
use tokio::time::{timeout, Duration};

fn mk_slot(id: &str, alias: &str, role: RoleKind, priority: u8) -> AgentSlot {
    AgentSlot {
        slot_id: id.to_string(),
        alias: alias.to_string(),
        display_name: format!("{}-{alias}", role.label_zh()),
        model_id: "[Gateway] gemini-4-neon-xhigh".to_string(),
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

#[test]
fn detects_gateway_503_no_available_accounts_as_quota_exhausted() {
    assert!(is_quota_error_text(
        "Error 503, Message: No available Gemini accounts: no available accounts"
    ));
    assert!(is_quota_error_text("Gateway returned account pool exhausted"));
    assert!(is_quota_error_text("upstream capacity exhausted, retry later"));
    assert!(!is_quota_error_text("rustc failed with syntax error"));
}

#[test]
fn architect_planning_prompt_enforces_lightweight_recon_and_plain_chinese_rules() {
    let slots = vec![mk_slot("arch-1", "arch", RoleKind::Architect, 1)];
    let prompt = build_architect_planning_prompt(
        "实现导出中心",
        "D:\\work\\demo",
        &slots,
        &[],
        &[],
        None,
    );
    assert!(prompt.contains("12~18"));
    assert!(prompt.contains("严禁在规划阶段逐字通读全仓"));
    assert!(prompt.contains("通俗大白话"));
    assert!(prompt.contains("user_plain_explanation"));
    assert!(prompt.contains("回复 1"));
}

#[test]
fn live_log_sink_and_stream_extractor_write_utf8_no_bom_logs_and_emit_channel() {
    let temp = std::env::temp_dir().join(format!(
        "agy-conductor-livelog-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let slots = vec![mk_slot("coder-1", "neon", RoleKind::Coder, 1)];
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = LiveLogSink::new(temp.clone(), Some(tx), &slots);

    let init_line = r#"{"event":"init","conversation_id":"conv-77","init":{"model":"[Gateway] gemini-4-neon-xhigh"}}"#;
    let think_line = r#"{"event":"step_update","step_update":{"step_index":1,"step_type":"thinking","thinking":"先检查入口导出结构再修改"}}"#;
    let tool_line = r#"{"event":"step_update","step_update":{"step_index":2,"step_type":"tool","tool_name":"edit_file","tool_info":{"name":"edit_file","parameters":{"Path":"D:\\work\\demo\\src\\main.rs"}}}}"#;
    let result_line = r#"{"event":"result","result":{"conversation_id":"conv-77","status":"SUCCESS","duration_seconds":3.5}}"#;

    for raw in [init_line, think_line, tool_line, result_line] {
        for (kind, text) in extract_live_stream_lines(raw) {
            sink.append_for_slot("coder-1", kind, text);
        }
    }

    let mut collected = Vec::new();
    while let Ok(item) = rx.try_recv() {
        collected.push(item);
    }
    assert!(collected.iter().any(|l| l.kind == LiveStreamKind::System));
    assert!(collected.iter().any(|l| l.kind == LiveStreamKind::Thinking));
    assert!(collected.iter().any(|l| l.kind == LiveStreamKind::ToolCall));
    assert!(collected.iter().any(|l| l.kind == LiveStreamKind::Result));

    let all_bytes = fs::read(log_file_for_slot(&temp, None)).unwrap();
    let slot_bytes = fs::read(log_file_for_slot(&temp, Some("coder-1"))).unwrap();
    assert!(!all_bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "live-all.log 不应含 UTF-8 BOM");
    assert!(!slot_bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "live-coder-1.log 不应含 UTF-8 BOM");
    let slot_text = String::from_utf8(slot_bytes).unwrap();
    assert!(slot_text.contains("[@neon]"));
    assert!(slot_text.contains("[工具]"));
    assert!(slot_text.contains("src/main.rs"));

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn terminal_wrapper_generates_utf8_no_bom_scripts_and_conflict_free_wt_args() {
    let temp = std::env::temp_dir().join(format!(
        "agy-conductor-termwrap-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let room_dir = temp.join("room-1");
    fs::create_dir_all(&room_dir).unwrap();

    let runtime = ResolvedAgyRuntime {
        strategy: AgyLaunchStrategy::DirectBinary {
            agy_exe: PathBuf::from("C:\\tools\\agy.exe"),
        },
        display_path: PathBuf::from("C:\\tools\\agy.exe"),
    };
    let mut slot = mk_slot("coder-1", "neon", RoleKind::Coder, 1);
    slot.conversation_id = Some("conv-live-001".to_string());

    // 1. watch_live = true 实时追踪流模式
    let live_opts = TerminalLaunchOptions {
        workspace_dir: temp.clone(),
        room_dir: Some(room_dir.clone()),
        mount_mcp: false,
        attach_conversation: false,
        watch_live: true,
    };
    let live_wrap = prepare_terminal_wrapper(&runtime, Some(&slot), &live_opts, &temp).unwrap();
    let ps1_bytes = fs::read(&live_wrap.ps1_path).unwrap();
    let cmd_bytes = fs::read(&live_wrap.cmd_path).unwrap();
    assert!(!ps1_bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert!(!cmd_bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    let ps1_text = String::from_utf8(ps1_bytes).unwrap();
    assert!(ps1_text.contains("Get-Content -LiteralPath $logPath -Encoding UTF8 -Wait -Tail 80"));
    assert!(live_wrap.log_path.as_ref().unwrap().exists());

    // 2. watch_live = false 交互式 CLI 接管模式（挂载 MCP）
    let cli_opts = TerminalLaunchOptions {
        workspace_dir: temp.clone(),
        room_dir: Some(room_dir.clone()),
        mount_mcp: true,
        attach_conversation: true,
        watch_live: false,
    };
    let cli_wrap = prepare_terminal_wrapper(&runtime, Some(&slot), &cli_opts, &temp).unwrap();
    let wt_args = build_wt_args_for_wrappers(&[live_wrap, cli_wrap], &temp);
    assert!(
        !wt_args.contains(&"cmd.exe".to_string())
            && !wt_args.contains(&"/D".to_string())
            && !wt_args.contains(&"/K".to_string()),
        "wt.exe 必须直调 PowerShell 绕开 cmd.exe /K 陷阱: {wt_args:?}"
    );
    assert!(wt_args.contains(&"-NoLogo".to_string()));
    assert!(wt_args.contains(&"-NoExit".to_string()));
    assert!(wt_args.contains(&"-ExecutionPolicy".to_string()));
    assert!(wt_args.contains(&"Bypass".to_string()));
    assert!(wt_args.contains(&"-File".to_string()));
    assert!(wt_args.contains(&"split-pane".to_string()));
    assert!(
        !wt_args.iter().any(|a| a == "--model" || a == "--settings" || a == "--conversation"),
        "wt.exe 顶层参数绝不能出现裸传的 --model/--settings/--conversation: {wt_args:?}"
    );

    // 3. 验证含 `;`、`&`、`()` 及换行的目录和标题均被正确转义且保留特殊路径字符
    let tricky_dir = temp.join("a;b & (demo)");
    fs::create_dir_all(&tricky_dir).unwrap();
    let mut tricky_slot = slot.clone();
    tricky_slot.display_name = "审查;A&B (1)\n换行".to_string();
    let tricky_wrap =
        prepare_terminal_wrapper(&runtime, Some(&tricky_slot), &cli_opts, &tricky_dir).unwrap();
    let tricky_args = build_wt_args_for_wrappers(&[tricky_wrap], &tricky_dir);
    assert_eq!(escape_wt_arg("x;y\nz"), "x\\;y z");
    assert!(
        tricky_args
            .iter()
            .any(|a| a.contains("a\\;b & (demo)") && !a.contains("a;b")),
        "工作目录中的分号必须转义为 \\; 且保留 & 与 (): {tricky_args:?}"
    );
    assert!(
        tricky_args
            .iter()
            .any(|a| a.contains("审查\\;A&B (1) 换行") && !a.contains('\n')),
        "标题中的分号必须转义为 \\; 且剥离换行符: {tricky_args:?}"
    );

    let _ = fs::remove_dir_all(&temp);
}

struct SessionSpyRunner {
    first_conv_by_slot: Mutex<HashMap<String, Option<String>>>,
}

impl AgyRunner for SessionSpyRunner {
    fn run_turn<'a>(
        &'a self,
        req: AgentTurnRequest,
        _act_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            {
                let mut map = self.first_conv_by_slot.lock().unwrap();
                map.entry(req.slot_id.clone())
                    .or_insert_with(|| req.conversation_id.clone());
            }
            let role_file = req.schema_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let conv = req
                .conversation_id
                .clone()
                .unwrap_or_else(|| format!("new-{}", req.slot_id));

            if role_file.contains("architect") {
                let structured = if req.prompt.contains("全局终验门禁") {
                    json!({
                        "action": "FINAL_ACCEPT",
                        "chat_message": "验收通过",
                        "architecture_notes": "ok",
                        "tasks": [],
                        "verification_commands": []
                    })
                } else {
                    json!({
                        "action": "DISPATCH_PLAN",
                        "chat_message": "拆解 T1",
                        "architecture_notes": "ok",
                        "tasks": [{
                            "task_id": "T1",
                            "title": "验证会话沿用",
                            "description": "ok",
                            "owned_paths": ["src/"],
                            "depends_on": [],
                            "preferred_coder_alias": null,
                            "acceptance_criteria": ["ok"]
                        }],
                        "verification_commands": []
                    })
                };
                return Ok(AgentTurnResponse {
                    conversation_id: Some(conv),
                    structured_json: structured,
                    raw_response: String::new(),
                    duration_seconds: 0.1,
                    tool_steps: 1,
                    usage: TokenUsage::default(),
                    peak_context_tokens: 0,
                    switched_to_fresh_session: false,
                });
            }

            if role_file.contains("coder") {
                return Ok(AgentTurnResponse {
                    conversation_id: Some(conv),
                    structured_json: json!({
                        "status": "SUBMIT_FOR_REVIEW",
                        "chat_message": "done",
                        "modified_files": ["src/lib.rs"],
                        "self_check_result": "ok",
                        "blocker_reason": null
                    }),
                    raw_response: String::new(),
                    duration_seconds: 0.1,
                    tool_steps: 1,
                    usage: TokenUsage::default(),
                    peak_context_tokens: 0,
                    switched_to_fresh_session: false,
                });
            }

            Ok(AgentTurnResponse {
                conversation_id: Some(conv),
                structured_json: json!({
                    "verdict": "APPROVED",
                    "chat_message": "ok",
                    "issues": [],
                    "commands_verified": []
                }),
                raw_response: String::new(),
                duration_seconds: 0.1,
                tool_steps: 1,
                usage: TokenUsage::default(),
                peak_context_tokens: 0,
                switched_to_fresh_session: false,
            })
        })
    }
}

#[tokio::test]
async fn workspace_role_session_reuse_injects_history_conversation_id_on_first_turn() {
    let temp = std::env::temp_dir().join(format!(
        "agy-conductor-reuse-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let workspace_dir = temp.join("my-project");
    fs::create_dir_all(&workspace_dir).unwrap();
    let ws_str = workspace_dir.to_string_lossy().to_string();

    let storage = AppStorage::with_base_dir(temp.join("appdata"));
    storage.ensure_dirs().unwrap();
    let room_dir = storage.create_room_dir("room-reuse").unwrap();
    let schemas = write_role_schemas(&storage.schemas_dir()).unwrap();

    let saved = vec![
        WorkspaceRoleSession {
            workspace: ws_str.clone(),
            workspace_key: String::new(),
            slot_id: "arch-1".to_string(),
            alias: "arch".to_string(),
            display_name: "总架构师".to_string(),
            role: RoleKind::Architect,
            model_id: "[Gateway] gemini-4-carbon-high".to_string(),
            conversation_id: "conv-prev-arch".to_string(),
            last_goal: "上次重构".to_string(),
            room_id: Some("room-old".to_string()),
            updated_at: "2026-11-20T10:00:00+08:00".to_string(),
            source: SESSION_SOURCE_CONDUCTOR.to_string(),
            context_tokens: 0,
            healthy_for_reuse: true,
            unhealthy_reason: None,
        },
        WorkspaceRoleSession {
            workspace: ws_str.clone(),
            workspace_key: String::new(),
            slot_id: "coder-1".to_string(),
            alias: "neon".to_string(),
            display_name: "编码程序员".to_string(),
            role: RoleKind::Coder,
            model_id: "[Gateway] gemini-4-neon-xhigh".to_string(),
            conversation_id: "conv-prev-coder".to_string(),
            last_goal: "上次重构".to_string(),
            room_id: Some("room-old".to_string()),
            updated_at: "2026-11-20T10:05:00+08:00".to_string(),
            source: SESSION_SOURCE_CONDUCTOR.to_string(),
            context_tokens: 0,
            healthy_for_reuse: true,
            unhealthy_reason: None,
        },
        WorkspaceRoleSession {
            workspace: ws_str.clone(),
            workspace_key: String::new(),
            slot_id: "rev-1".to_string(),
            alias: "rev".to_string(),
            display_name: "代码审查员".to_string(),
            role: RoleKind::Reviewer,
            model_id: "[Gateway] gemini-4-carbon-high".to_string(),
            conversation_id: "conv-prev-rev".to_string(),
            last_goal: "上次重构".to_string(),
            room_id: Some("room-old".to_string()),
            updated_at: "2026-11-20T10:10:00+08:00".to_string(),
            source: SESSION_SOURCE_CONDUCTOR.to_string(),
            context_tokens: 0,
            healthy_for_reuse: true,
            unhealthy_reason: None,
        },
    ];
    upsert_workspace_role_sessions(&storage, &saved).unwrap();

    let discovered = discover_workspace_role_sessions(&storage, &ws_str, Some(&temp.join("no-brain")));
    let mut slots = vec![
        mk_slot("arch-1", "arch", RoleKind::Architect, 1),
        mk_slot("coder-1", "neon", RoleKind::Coder, 1),
        mk_slot("rev-1", "rev", RoleKind::Reviewer, 1),
    ];

    // 显式跳过审查员沿用，其余两个角色自动沿用历史会话
    let mut explicit = HashMap::new();
    explicit.insert("rev-1".to_string(), "__new__".to_string());
    let reused = apply_session_reuse_for_slots(&mut slots, &discovered, true, &explicit);
    assert_eq!(reused.len(), 2);

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (_ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(SessionSpyRunner {
        first_conv_by_slot: Mutex::new(HashMap::new()),
    });
    let engine = OrchestratorEngine::new(
        EngineConfig {
            room_id: "room-reuse".to_string(),
            room_dir,
            workspace_dir,
            goal: "沿用历史项目记忆继续迭代".to_string(),
            permission_mode: PermissionMode::AcceptEdits,
            enable_sandbox: false,
            max_rework_rounds: 3,
            turn_timeout_secs: 30,
            dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
            slots,
            schemas,
        },
        runner.clone(),
        evt_tx,
        ctrl_rx,
    );
    let handle = tokio::spawn(async move { engine.run().await });

    timeout(Duration::from_secs(8), async {
        while let Some(ev) = evt_rx.recv().await {
            if matches!(ev, EngineEvent::Completed(_)) {
                break;
            }
        }
    })
    .await
    .expect("会话沿用运行应完成");
    handle.await.unwrap();

    let seen = runner.first_conv_by_slot.lock().unwrap();
    assert_eq!(seen.get("arch-1").and_then(|o| o.as_deref()), Some("conv-prev-arch"));
    assert_eq!(seen.get("coder-1").and_then(|o| o.as_deref()), Some("conv-prev-coder"));
    assert_eq!(seen.get("rev-1").and_then(|o| o.as_deref()), None);

    let _ = fs::remove_dir_all(&temp);
}
