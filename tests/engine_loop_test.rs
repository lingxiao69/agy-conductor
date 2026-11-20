use agy_conductor::agy::runner::{
    AgentActivityUpdate, AgentTurnError, AgentTurnRequest, AgentTurnResponse, AgyRunner,
    TurnFuture,
};
use agy_conductor::agy::stream_parser::{AgentErrorKind, TokenUsage};
use agy_conductor::billing::SlotUsageStats;
use agy_conductor::orchestrator::engine::{
    EngineConfig, EngineEvent, OrchestratorEngine,
};
use agy_conductor::orchestrator::schemas::write_role_schemas;
use agy_conductor::orchestrator::types::{
    AgentSlot, PermissionMode, RoleKind, SameRoleDispatchMode, SlotRunState, TaskStatus,
};
use agy_conductor::storage::store::AppStorage;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, timeout, Duration};

fn mk_slot(id: &str, alias: &str, name: &str, model: &str, role: RoleKind, priority: u8) -> AgentSlot {
    AgentSlot {
        slot_id: id.to_string(),
        alias: alias.to_string(),
        display_name: name.to_string(),
        model_id: model.to_string(),
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

fn ok_turn(conv_id: String, structured_json: serde_json::Value, dur: f64, steps: u32) -> AgentTurnResponse {
    AgentTurnResponse {
        conversation_id: Some(conv_id),
        structured_json,
        raw_response: String::new(),
        duration_seconds: dur,
        tool_steps: steps,
        usage: TokenUsage::default(),
        peak_context_tokens: 0,
        switched_to_fresh_session: false,
    }
}

fn default_test_slots() -> Vec<AgentSlot> {
    vec![
        mk_slot("arch-carbon", "carbon-arch", "总架构师 · Carbon", "[Gateway] gemini-4-carbon-high", RoleKind::Architect, 1),
        mk_slot("coder-neon", "neon", "编码程序员 · Neon", "[Gateway] gemini-4-neon-xhigh", RoleKind::Coder, 1),
        mk_slot("coder-hydrogen", "hydrogen", "编码程序员 · Hydrogen", "[Gateway] gemini-4-hydrogen-xhigh", RoleKind::Coder, 2),
        mk_slot("rev-carbon", "carbon-rev", "代码审查员 · Carbon", "[Gateway] gemini-4-carbon-high", RoleKind::Reviewer, 1),
    ]
}

#[derive(Default)]
struct FullLoopMockState {
    t2_review_attempts: u32,
    t2_coders_seen: Vec<(String, Option<String>)>,
    t1_coders_seen: Vec<String>,
    max_concurrent_coders: usize,
    active_coders: usize,
}

struct FullLoopMockRunner {
    state: Mutex<FullLoopMockState>,
}

impl AgyRunner for FullLoopMockRunner {
    fn run_turn<'a>(
        &'a self,
        request: AgentTurnRequest,
        activity_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            let conv_id = request
                .conversation_id
                .clone()
                .unwrap_or_else(|| format!("conv-{}", request.slot_id));

            let _ = activity_tx.send(AgentActivityUpdate {
                slot_id: request.slot_id.clone(),
                summary: format!("正在执行 ({})", request.slot_id),
                tool_steps: 2,
                conversation_id: Some(conv_id.clone()),
                in_flight_usage: None,
                peak_context_tokens: 0,
            });

            if request.slot_id == "arch-carbon" {
                let structured = if request.prompt.contains("全局终验门禁") {
                    json!({
                        "action": "FINAL_ACCEPT",
                        "chat_message": "经终验确认，T1 与 T2 全部符合长期目标，验收通过！",
                        "architecture_notes": "终验通过",
                        "verification_commands": ["cargo test"],
                        "tasks": []
                    })
                } else {
                    json!({
                        "action": "DISPATCH_PLAN",
                        "chat_message": "@neon @hydrogen 已拆解 T1（核心模型）与 T2（对账模块）两个无冲突任务。",
                        "architecture_notes": "严格遵守单文件 <500 行与 UTF-8 无 BOM",
                        "verification_commands": ["cargo test"],
                        "tasks": [
                            {
                                "task_id": "T1",
                                "title": "核心领域模型",
                                "description": "实现订单结构与基础类型",
                                "owned_paths": ["src/core/"],
                                "depends_on": [],
                                "preferred_coder_alias": null,
                                "acceptance_criteria": ["通过单元测试"]
                            },
                            {
                                "task_id": "T2",
                                "title": "账单核销算法",
                                "description": "实现核销与负金额冲正",
                                "owned_paths": ["src/reconcile/"],
                                "depends_on": [],
                                "preferred_coder_alias": null,
                                "acceptance_criteria": ["支持负金额冲正与回归测试"]
                            }
                        ]
                    })
                };
                return Ok(ok_turn(conv_id, structured, 1.2, 2));
            }

            if request.slot_id.starts_with("coder-") {
                {
                    let mut st = self.state.lock().unwrap();
                    st.active_coders += 1;
                    if st.active_coders > st.max_concurrent_coders {
                        st.max_concurrent_coders = st.active_coders;
                    }
                    if request.prompt.contains("当前分配子任务：T1") {
                        st.t1_coders_seen.push(request.slot_id.clone());
                    } else if request.prompt.contains("当前分配子任务：T2") {
                        st.t2_coders_seen
                            .push((request.slot_id.clone(), request.conversation_id.clone()));
                    }
                }
                sleep(Duration::from_millis(45)).await;
                {
                    self.state.lock().unwrap().active_coders -= 1;
                }

                let file = if request.prompt.contains("T1") {
                    "src/core/model.rs"
                } else {
                    "src/reconcile/engine.rs"
                };
                return Ok(ok_turn(
                    conv_id,
                    json!({
                        "status": "SUBMIT_FOR_REVIEW",
                        "chat_message": format!("@carbon-rev 已完成 {file} 实现与自测，请审查。"),
                        "modified_files": [file],
                        "self_check_result": "cargo test passed",
                        "blocker_reason": null
                    }),
                    2.0,
                    4,
                ));
            }

            // Reviewer
            let is_t2 = request.prompt.contains("待审查子任务：T2");
            let reject = if is_t2 {
                let mut st = self.state.lock().unwrap();
                st.t2_review_attempts += 1;
                st.t2_review_attempts == 1
            } else {
                false
            };

            let structured = if reject {
                json!({
                    "verdict": "CHANGES_REQUESTED",
                    "chat_message": "@hydrogen 审查发现 src/reconcile/engine.rs 未处理负金额冲正，请修复后重提。",
                    "commands_verified": ["cargo check"],
                    "issues": [{
                        "severity": "MAJOR",
                        "file_path": "src/reconcile/engine.rs",
                        "line_hint": "142",
                        "detail": "未处理负金额冲正边界"
                    }]
                })
            } else {
                json!({
                    "verdict": "APPROVED",
                    "chat_message": "代码与单测核验通过，放行！",
                    "commands_verified": ["cargo test"],
                    "issues": []
                })
            };

            Ok(ok_turn(conv_id, structured, 1.5, 3))
        })
    }
}

#[tokio::test]
async fn full_closed_loop_plan_parallel_code_reject_sticky_rework_and_final_accept() {
    let tmp = std::env::temp_dir().join(format!("agy-engine-loop-{}", uuid::Uuid::new_v4()));
    let ws = tmp.join("project");
    std::fs::create_dir_all(&ws).unwrap();
    let storage = AppStorage::with_base_dir(tmp.join("data"));
    let room_dir = storage.create_room_dir("room-test-1").unwrap();
    let schemas = write_role_schemas(&storage.schemas_dir()).unwrap();

    let mock = Arc::new(FullLoopMockRunner {
        state: Mutex::new(FullLoopMockState::default()),
    });
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (_ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();

    let cfg = EngineConfig {
        room_id: "room-test-1".to_string(),
        room_dir: room_dir.clone(),
        workspace_dir: ws,
        goal: "实现核心模型与账单核销模块".to_string(),
        permission_mode: PermissionMode::AcceptEdits,
        enable_sandbox: false,
        max_rework_rounds: 8,
        turn_timeout_secs: 60,
        dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
        slots: default_test_slots(),
        schemas,
    };

    let engine = OrchestratorEngine::new(cfg, mock.clone(), evt_tx, ctrl_rx);
    let handle = tokio::spawn(async move { engine.run().await });

    let mut completed = None;
    let mut final_tasks = Vec::new();
    let deadline = timeout(Duration::from_secs(10), async {
        while let Some(evt) = evt_rx.recv().await {
            match evt {
                EngineEvent::TasksUpdated(t) => final_tasks = t,
                EngineEvent::Completed(summary) => {
                    completed = Some(summary);
                    break;
                }
                _ => {}
            }
        }
    })
    .await;

    assert!(deadline.is_ok(), "engine loop timed out");
    let _ = handle.await;

    let summary = completed.expect("must emit Completed event");
    assert_eq!(summary.total_tasks, 2);
    assert_eq!(summary.total_rework_rounds, 1);
    assert!(summary.verified_commands.contains(&"cargo test".to_string()));
    assert!(final_tasks.iter().all(|t| t.status == TaskStatus::Approved));

    let st = mock.state.lock().unwrap();
    assert_eq!(st.max_concurrent_coders, 2, "P1 and P2 should run disjoint tasks in parallel");
    assert_eq!(st.t1_coders_seen, vec!["coder-neon".to_string()]);
    assert_eq!(st.t2_coders_seen.len(), 2, "T2 should be coded once and reworked once");
    // Sticky rework: T2's rework must be executed by the exact same coder with preserved conversation_id!
    assert_eq!(st.t2_coders_seen[0].0, "coder-hydrogen");
    assert_eq!(st.t2_coders_seen[1].0, "coder-hydrogen");
    assert_eq!(
        st.t2_coders_seen[1].1.as_deref(),
        Some("conv-coder-hydrogen"),
        "rework turn must reuse original coder conversation_id"
    );

    let persisted = AppStorage::read_board_messages(&room_dir).unwrap();
    assert!(persisted.len() >= 6, "board.jsonl should record the full group conversation");

    let _ = std::fs::remove_dir_all(&tmp);
}

struct QuotaFailoverMockRunner {
    calls: Mutex<HashMap<String, u32>>,
}

impl AgyRunner for QuotaFailoverMockRunner {
    fn run_turn<'a>(
        &'a self,
        request: AgentTurnRequest,
        _activity_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        _cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            {
                let mut map = self.calls.lock().unwrap();
                *map.entry(request.slot_id.clone()).or_insert(0) += 1;
            }
            if request.slot_id == "coder-neon" {
                return Err(AgentTurnError {
                    kind: AgentErrorKind::QuotaExhausted,
                    message: "429 RESOURCE_EXHAUSTED".to_string(),
                    conversation_id: None,
                    partial_usage: None,
                    peak_context_tokens: 0,
                });
            }
            if request.slot_id == "arch-carbon" {
                let structured = if request.prompt.contains("全局终验门禁") {
                    json!({
                        "action": "FINAL_ACCEPT",
                        "chat_message": "终验通过",
                        "architecture_notes": "",
                        "verification_commands": ["cargo test"],
                        "tasks": []
                    })
                } else {
                    json!({
                        "action": "DISPATCH_PLAN",
                        "chat_message": "@neon 开始 T1",
                        "architecture_notes": "",
                        "verification_commands": ["cargo test"],
                        "tasks": [{
                            "task_id": "T1",
                            "title": "导出 CSV",
                            "description": "实现 CSV 导出",
                            "owned_paths": ["src/csv.rs"],
                            "depends_on": [],
                            "preferred_coder_alias": null,
                            "acceptance_criteria": ["测试通过"]
                        }]
                    })
                };
                return Ok(ok_turn("conv-arch".to_string(), structured, 0.5, 1));
            }
            if request.slot_id == "coder-hydrogen" {
                return Ok(ok_turn(
                    "conv-h".to_string(),
                    json!({
                        "status": "SUBMIT_FOR_REVIEW",
                        "chat_message": "@carbon-rev 已接力完成 T1",
                        "modified_files": ["src/csv.rs"],
                        "self_check_result": "ok",
                        "blocker_reason": null
                    }),
                    0.8,
                    2,
                ));
            }
            Ok(ok_turn(
                "conv-rev".to_string(),
                json!({
                    "verdict": "APPROVED",
                    "chat_message": "通过",
                    "commands_verified": ["cargo test"],
                    "issues": []
                }),
                0.5,
                1,
            ))
        })
    }
}

#[tokio::test]
async fn strict_primary_quota_exhausted_automatically_fails_over_to_p2_and_completes() {
    let tmp = std::env::temp_dir().join(format!("agy-failover-{}", uuid::Uuid::new_v4()));
    let ws = tmp.join("project");
    std::fs::create_dir_all(&ws).unwrap();
    let storage = AppStorage::with_base_dir(tmp.join("data"));
    let room_dir = storage.create_room_dir("room-failover").unwrap();
    let schemas = write_role_schemas(&storage.schemas_dir()).unwrap();

    let mock = Arc::new(QuotaFailoverMockRunner {
        calls: Mutex::new(HashMap::new()),
    });
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (_ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();

    let cfg = EngineConfig {
        room_id: "room-failover".to_string(),
        room_dir,
        workspace_dir: ws,
        goal: "支持 CSV 导出".to_string(),
        permission_mode: PermissionMode::AcceptEdits,
        enable_sandbox: false,
        max_rework_rounds: 5,
        turn_timeout_secs: 60,
        dispatch_mode: SameRoleDispatchMode::StrictPrimaryFailover,
        slots: default_test_slots(),
        schemas,
    };

    let engine = OrchestratorEngine::new(cfg, mock.clone(), evt_tx, ctrl_rx);
    let handle = tokio::spawn(async move { engine.run().await });

    let mut completed = false;
    let mut neon_cooled = false;
    let res = timeout(Duration::from_secs(10), async {
        while let Some(evt) = evt_rx.recv().await {
            match evt {
                EngineEvent::SlotStateChanged { slot_id, state, .. }
                    if slot_id == "coder-neon" && state == SlotRunState::QuotaCooling =>
                {
                    neon_cooled = true;
                }
                EngineEvent::Completed(_) => {
                    completed = true;
                    break;
                }
                _ => {}
            }
        }
    })
    .await;

    assert!(res.is_ok());
    let _ = handle.await;
    assert!(neon_cooled, "P1 Neon must enter QuotaCooling on 429");
    assert!(completed, "P2 Hydrogen must finish task so project completes");
    let calls = mock.calls.lock().unwrap();
    assert_eq!(calls.get("coder-neon"), Some(&1));
    assert_eq!(calls.get("coder-hydrogen"), Some(&1));

    let _ = std::fs::remove_dir_all(&tmp);
}
