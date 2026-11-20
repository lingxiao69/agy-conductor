mod v6_support;

use agy_conductor::agy::live_log::{LiveStreamKind, LiveTerminalLine};
use agy_conductor::billing::SlotUsageStats;
use agy_conductor::ipc::run_events::{
    apply_engine_event_for_generation, apply_live_line_for_generation,
};
use agy_conductor::ipc::state::AppCoreState;
use agy_conductor::orchestrator::engine::{EngineControl, EngineEvent, OrchestratorEngine};
use agy_conductor::orchestrator::engine_turns::make_system_message;
use agy_conductor::orchestrator::schemas::PlannedTaskSpec;
use agy_conductor::orchestrator::task_graph::{merge_planned_tasks_safely, validate_task_dag};
use agy_conductor::orchestrator::types::{MessageBadge, RoleKind, RunPhase, TaskStatus};
use agy_conductor::orchestrator::workspace_guard::{
    dependencies_satisfied, paths_conflict_in_workspace,
};
use agy_conductor::storage::store::AppStorage;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Mutex as AsyncMutex, Notify};
use tokio::time::{timeout, Duration};
use v6_support::{
    mk_engine_config, mk_external_cli_msg, mk_slot, mk_task, setup_env, ContractChangeRunner,
    CyclicThenFixedDagRunner, InterjectionSwapRunner, SimplePlanRunner,
};

/// 场景 1：架构师槽位初始不可用 -> 进入 WaitingUser -> 恢复槽位并 Resume 后成功触发初始规划不再假死。
#[tokio::test]
async fn architect_unavailable_then_resume_triggers_initial_plan() {
    let (temp, room_dir, schemas) = setup_env("arch-resume");
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(SimplePlanRunner {
        arch_calls: AtomicUsize::new(0),
    });

    let mut arch_disabled = mk_slot("arch-1", "arch", RoleKind::Architect, 1);
    arch_disabled.enabled = false;
    let coder = mk_slot("coder-1", "coder", RoleKind::Coder, 1);
    let rev = mk_slot("rev-1", "rev", RoleKind::Reviewer, 1);

    let cfg = mk_engine_config(
        "room-arch-resume",
        room_dir.clone(),
        temp.clone(),
        "恢复初始规划验证",
        vec![arch_disabled.clone(), coder.clone(), rev.clone()],
        schemas,
    );
    let engine = OrchestratorEngine::new(cfg, runner.clone(), evt_tx, ctrl_rx);
    let handle = tokio::spawn(async move { engine.run().await });

    let mut restored = false;
    let mut completed = false;
    timeout(Duration::from_secs(8), async {
        while let Some(ev) = evt_rx.recv().await {
            match ev {
                EngineEvent::PhaseChanged(RunPhase::WaitingUser) if !restored => {
                    restored = true;
                    let mut arch_ok = arch_disabled.clone();
                    arch_ok.enabled = true;
                    ctrl_tx
                        .send(EngineControl::UpdateSlots(vec![arch_ok, coder.clone(), rev.clone()]))
                        .unwrap();
                    ctrl_tx.send(EngineControl::Resume).unwrap();
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
    .expect("恢复架构师槽位后必须完成初始规划与全流程");
    handle.await.unwrap();

    assert!(restored && completed);
    assert!(runner.arch_calls.load(Ordering::SeqCst) >= 2);
    let snap = AppStorage::read_room_snapshot(&room_dir).expect("终态快照必须异步落盘完成");
    assert_eq!(snap.phase, "验收已完成");
    let _ = std::fs::remove_dir_all(&temp);
}

/// 场景 2：用户插话在架构师槽位不可用以及被 UpdateSlots 热切换取消后，pending_user_notes 零丢失并由新架构师消费。
#[tokio::test]
async fn user_interjection_preserved_on_unavailable_and_hot_swap() {
    let (temp, room_dir, schemas) = setup_env("interject-preserve");
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(InterjectionSwapRunner {
        arch1_started: Notify::new(),
        arch1_canceled: AtomicBool::new(false),
        arch2_seen_prompts: Mutex::new(Vec::new()),
    });

    let mut arch1 = mk_slot("arch-1", "arch1", RoleKind::Architect, 1);
    arch1.enabled = false;
    let coder = mk_slot("coder-1", "coder", RoleKind::Coder, 1);
    let rev = mk_slot("rev-1", "rev", RoleKind::Reviewer, 1);

    let cfg = mk_engine_config(
        "room-interject",
        room_dir,
        temp.clone(),
        "验证插话保留",
        vec![arch1.clone(), coder.clone(), rev.clone()],
        schemas,
    );
    let engine = OrchestratorEngine::new(cfg, runner.clone(), evt_tx, ctrl_rx);
    let handle = tokio::spawn(async move { engine.run().await });

    timeout(Duration::from_secs(5), async {
        while let Some(ev) = evt_rx.recv().await {
            if matches!(ev, EngineEvent::PhaseChanged(RunPhase::WaitingUser)) {
                break;
            }
        }
    })
    .await
    .unwrap();

    ctrl_tx
        .send(EngineControl::PostUserMessage("热切换不能丢失的指令".to_string()))
        .unwrap();
    let mut arch1_enabled = arch1.clone();
    arch1_enabled.enabled = true;
    ctrl_tx
        .send(EngineControl::UpdateSlots(vec![arch1_enabled, coder.clone(), rev.clone()]))
        .unwrap();
    ctrl_tx.send(EngineControl::Resume).unwrap();

    timeout(Duration::from_secs(5), runner.arch1_started.notified())
        .await
        .expect("arch-1 应启动处理用户插话");

    let mut arch2 = mk_slot("arch-2", "arch2", RoleKind::Architect, 1);
    arch2.model_id = "[Gateway] gemini-4-carbon*".to_string();
    ctrl_tx
        .send(EngineControl::UpdateSlots(vec![arch2, coder, rev]))
        .unwrap();

    timeout(Duration::from_secs(8), async {
        while let Some(ev) = evt_rx.recv().await {
            if matches!(ev, EngineEvent::Completed(_)) {
                break;
            }
        }
    })
    .await
    .expect("热切换为 arch-2 后必须消费保留的插话并完成");
    handle.await.unwrap();

    assert!(runner.arch1_canceled.load(Ordering::SeqCst));
    let prompts = runner.arch2_seen_prompts.lock().unwrap();
    assert!(
        prompts.iter().any(|p| p.contains("热切换不能丢失的指令")),
        "新架构师必须收到完整的原始插话，实际: {prompts:?}"
    );
    let _ = std::fs::remove_dir_all(&temp);
}

/// 场景 3：merge_planned_tasks_safely 在契约未变时不打断 Coding，契约变更时取消旧轮次并重置为 Pending 重派。
#[tokio::test]
async fn merge_planned_tasks_protects_active_contract_and_cancels_on_change() {
    let mut t1 = mk_task("T1", vec![], vec!["src/a.rs"], TaskStatus::Coding);
    t1.assigned_coder_slot_id = Some("coder-1".to_string());
    t1.rework_count = 2;
    let mut tasks = vec![t1];

    let unchanged = vec![PlannedTaskSpec {
        task_id: "T1".to_string(),
        title: "仅更新标题".to_string(),
        description: "Desc T1".to_string(),
        owned_paths: vec!["src/a.rs".to_string()],
        depends_on: vec![],
        preferred_coder_alias: Some("neon".to_string()),
        acceptance_criteria: vec!["pass".to_string()],
    }];
    let out1 = merge_planned_tasks_safely(&mut tasks, unchanged);
    assert!(out1.invalidated_task_ids.is_empty());
    assert_eq!(tasks[0].status, TaskStatus::Coding);
    assert_eq!(tasks[0].title, "仅更新标题");
    assert_eq!(tasks[0].assigned_coder_slot_id.as_deref(), Some("coder-1"));

    let changed = vec![PlannedTaskSpec {
        task_id: "T1".to_string(),
        title: "新契约".to_string(),
        description: "改了需求描述".to_string(),
        owned_paths: vec!["src/a.rs".to_string()],
        depends_on: vec![],
        preferred_coder_alias: None,
        acceptance_criteria: vec!["new-pass".to_string()],
    }];
    let out2 = merge_planned_tasks_safely(&mut tasks, changed);
    assert_eq!(out2.invalidated_task_ids, vec!["T1".to_string()]);
    assert_eq!(tasks[0].status, TaskStatus::Pending);
    assert!(tasks[0].assigned_coder_slot_id.is_none());
    assert_eq!(tasks[0].rework_count, 0);

    let (temp, room_dir, schemas) = setup_env("contract-change");
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(ContractChangeRunner {
        coder_round: AtomicUsize::new(0),
        first_coder_started: Notify::new(),
        first_coder_canceled: AtomicBool::new(false),
        second_coder_saw_new_contract: AtomicBool::new(false),
    });

    let cfg = mk_engine_config(
        "room-contract",
        room_dir,
        temp.clone(),
        "验证契约变更取消旧轮次",
        vec![
            mk_slot("arch-1", "arch", RoleKind::Architect, 1),
            mk_slot("coder-1", "coder", RoleKind::Coder, 1),
            mk_slot("rev-1", "rev", RoleKind::Reviewer, 1),
        ],
        schemas,
    );
    let engine = OrchestratorEngine::new(cfg, runner.clone(), evt_tx, ctrl_rx);
    let handle = tokio::spawn(async move { engine.run().await });

    timeout(Duration::from_secs(5), runner.first_coder_started.notified())
        .await
        .expect("首轮 Coder 应启动");
    ctrl_tx
        .send(EngineControl::PostUserMessage("请把 T1 改为输出 JSON".to_string()))
        .unwrap();

    timeout(Duration::from_secs(8), async {
        while let Some(ev) = evt_rx.recv().await {
            if matches!(ev, EngineEvent::Completed(_)) {
                break;
            }
        }
    })
    .await
    .expect("新契约应取消旧轮次并顺利完成");
    handle.await.unwrap();

    assert!(runner.first_coder_canceled.load(Ordering::SeqCst));
    assert!(runner.second_coder_saw_new_contract.load(Ordering::SeqCst));
    let _ = std::fs::remove_dir_all(&temp);
}

/// 场景 4：validate_task_dag 拦截环路与悬空依赖，并在引擎中自动重派架构师修正自愈。
#[tokio::test]
async fn validate_task_dag_blocks_cycles_and_missing_deps_and_self_heals() {
    let cyclic = vec![
        mk_task("T1", vec!["T2"], vec!["src/a.rs"], TaskStatus::Pending),
        mk_task("T2", vec!["T1"], vec!["src/b.rs"], TaskStatus::Pending),
    ];
    assert!(validate_task_dag(&cyclic).unwrap_err().contains("循环依赖"));

    let missing = vec![mk_task("T1", vec!["T99"], vec!["src/a.rs"], TaskStatus::Pending)];
    assert!(validate_task_dag(&missing).unwrap_err().contains("T99"));
    assert!(!dependencies_satisfied(&missing[0], &missing));

    let self_dep = vec![mk_task("T1", vec!["T1"], vec!["src/a.rs"], TaskStatus::Pending)];
    assert!(validate_task_dag(&self_dep).unwrap_err().contains("自依赖"));

    let (temp, room_dir, schemas) = setup_env("dag-heal");
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (_ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(CyclicThenFixedDagRunner {
        plan_attempts: AtomicUsize::new(0),
    });

    let cfg = mk_engine_config(
        "room-dag",
        room_dir,
        temp.clone(),
        "验证 DAG 环路拦截自愈",
        vec![
            mk_slot("arch-1", "arch", RoleKind::Architect, 1),
            mk_slot("coder-1", "coder", RoleKind::Coder, 1),
            mk_slot("rev-1", "rev", RoleKind::Reviewer, 1),
        ],
        schemas,
    );
    let engine = OrchestratorEngine::new(cfg, runner.clone(), evt_tx, ctrl_rx);
    let handle = tokio::spawn(async move { engine.run().await });

    let mut saw_dag_warn = false;
    let mut completed = false;
    timeout(Duration::from_secs(8), async {
        while let Some(ev) = evt_rx.recv().await {
            match ev {
                EngineEvent::ChatMessageAppended(m) if m.is_system && m.content.contains("循环依赖") => {
                    saw_dag_warn = true;
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
    .expect("架构师应自动修正 DAG 并完成全流程");
    handle.await.unwrap();

    assert!(saw_dag_warn && completed);
    assert_eq!(runner.plan_attempts.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(&temp);
}

/// 场景 5：workspace_guard 通配符截断、绝对路径剥离、`..` 归一化全面互斥验证。
#[test]
fn workspace_guard_normalizes_wildcards_abs_paths_and_dotdot() {
    let ws = Path::new("D:\\work\\demo");
    assert!(paths_conflict_in_workspace(&["src/**/*.rs".into()], &["src/agy/runner.rs".into()], Some(ws)));
    assert!(!paths_conflict_in_workspace(&["src/**/*.rs".into()], &["assets/js/main.js".into()], Some(ws)));
    assert!(paths_conflict_in_workspace(&["src/ipc/run_*.rs".into()], &["src/ipc/run_events.rs".into()], Some(ws)));
    assert!(paths_conflict_in_workspace(&["*.toml".into()], &["src/main.rs".into()], Some(ws)));
    assert!(paths_conflict_in_workspace(&["D:\\work\\demo\\src\\agy\\runner.rs".into()], &["src/agy/runner.rs".into()], Some(ws)));
    assert!(paths_conflict_in_workspace(&["\\\\?\\D:\\work\\demo\\src\\agy".into()], &["src/agy/runner.rs".into()], Some(ws)));
    assert!(!paths_conflict_in_workspace(&["D:\\work\\demo\\src\\agy\\runner.rs".into()], &["src/ipc/state.rs".into()], Some(ws)));
    assert!(paths_conflict_in_workspace(&["src/ipc/../agy/runner.rs".into()], &["src/agy/runner.rs".into()], Some(ws)));
    assert!(paths_conflict_in_workspace(&["../../outside.rs".into()], &["src/ipc/state.rs".into()], Some(ws)));
}

/// 场景 6：board.jsonl 混入非法 UTF-8 脏字节行后，read_board_messages 与引擎 poll_external_board_messages 均不卡死。
#[tokio::test]
async fn board_jsonl_bad_utf8_does_not_stall_external_poll_or_history_read() {
    let (temp, room_dir, schemas) = setup_env("bad-utf8");
    let board_path = room_dir.join("board.jsonl");

    let first = make_system_message("历史正常消息", MessageBadge::Info, None);
    AppStorage::append_board_message(&room_dir, &first).unwrap();
    {
        let mut f = OpenOptions::new().append(true).open(&board_path).unwrap();
        f.write_all(b"\xff\xfe{invalid-utf8}\n").unwrap();
    }
    let second = make_system_message("脏行之后依然可读", MessageBadge::Info, None);
    AppStorage::append_board_message(&room_dir, &second).unwrap();
    let loaded = AppStorage::read_board_messages(&room_dir).unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[1].content, "脏行之后依然可读");

    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let mut arch_off = mk_slot("arch-1", "arch", RoleKind::Architect, 1);
    arch_off.enabled = false;
    let cfg = mk_engine_config(
        "room-utf8",
        room_dir.clone(),
        temp.clone(),
        "验证坏字节不卡死轮询",
        vec![arch_off],
        schemas,
    );
    let engine = OrchestratorEngine::new(
        cfg,
        Arc::new(SimplePlanRunner { arch_calls: AtomicUsize::new(0) }),
        evt_tx,
        ctrl_rx,
    );
    let handle = tokio::spawn(async move { engine.run().await });

    timeout(Duration::from_secs(3), async {
        while let Some(ev) = evt_rx.recv().await {
            if matches!(ev, EngineEvent::PhaseChanged(RunPhase::WaitingUser)) {
                break;
            }
        }
    })
    .await
    .unwrap();

    {
        let mut f = OpenOptions::new().create(true).append(true).open(&board_path).unwrap();
        f.write_all(b"\x80\xff\xfe broken-bytes\n").unwrap();
    }
    let ext_msg = mk_external_cli_msg("ext-cli-msg-1", "外部终端汇报：已手工修复构建脚本");
    AppStorage::append_board_message(&room_dir, &ext_msg).unwrap();

    let mut received_ext = false;
    timeout(Duration::from_secs(5), async {
        while let Some(ev) = evt_rx.recv().await {
            if let EngineEvent::ChatMessageAppended(m) = ev {
                if m.id == "ext-cli-msg-1" {
                    received_ext = true;
                    break;
                }
            }
        }
    })
    .await
    .expect("坏字节行后仍必须轮询到后续合法外部 CLI 群消息");

    ctrl_tx.send(EngineControl::Stop).unwrap();
    handle.await.unwrap();
    assert!(received_ext);
    let _ = std::fs::remove_dir_all(&temp);
}

/// 场景 7：ResetRun 递增 run_generation 后，旧引擎延迟事件与日志被完全拦截；ApplyDefaultRoster 运行态同步。
#[tokio::test]
async fn reset_run_generation_isolates_stale_events_and_default_roster_syncs() {
    let temp = std::env::temp_dir().join(format!(
        "agy-v6-gen-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let storage = AppStorage::with_base_dir(temp.clone());
    storage.ensure_dirs().unwrap();

    let shared = Arc::new(AsyncMutex::new(AppCoreState::new(storage, None, None)));
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    let old_gen = {
        let mut g = shared.lock().await;
        g.run_generation = g.run_generation.wrapping_add(1);
        g.phase = RunPhase::Running;
        g.current_room_id = Some("room-old".to_string());
        g.current_room_dir = Some(temp.join("room-old"));
        g.active_engine_tx = Some(ctrl_tx);
        g.apply_default_roster();
        g.run_generation
    };
    assert!(matches!(ctrl_rx.try_recv(), Ok(EngineControl::UpdateDispatchMode(_))));
    assert!(matches!(ctrl_rx.try_recv(), Ok(EngineControl::UpdateSlots(_))));

    {
        let mut g = shared.lock().await;
        g.reset_run_state();
        assert_eq!(g.run_generation, old_gen + 1);
        assert!(g.current_room_id.is_none());
        assert!(g.current_room_dir.is_none());
        assert_eq!(g.phase, RunPhase::Idle);
    }

    let mut persisted = HashSet::new();
    assert!(apply_engine_event_for_generation(
        &shared, old_gen, EngineEvent::PhaseChanged(RunPhase::Stopped), &mut persisted
    ).await.is_none());
    assert!(apply_engine_event_for_generation(
        &shared, old_gen,
        EngineEvent::SlotUsageUpdated {
            slot_id: "arch-1".to_string(),
            usage: SlotUsageStats { total_tokens: 99999, cost_usd: 1.23, ..SlotUsageStats::default() },
        },
        &mut persisted,
    ).await.is_none());
    assert!(apply_engine_event_for_generation(
        &shared, old_gen,
        EngineEvent::ChatMessageAppended(make_system_message("旧会话残留", MessageBadge::Warning, None)),
        &mut persisted,
    ).await.is_none());
    assert!(!apply_live_line_for_generation(
        &shared, old_gen,
        LiveTerminalLine {
            seq: 1,
            timestamp: "12:00:00".to_string(), slot_id: "arch-1".to_string(),
            alias: "arch".to_string(), display_name: "总架构师".to_string(),
            kind: LiveStreamKind::System, text: "旧引擎延迟日志".to_string(),
        },
    ).await);

    let g = shared.lock().await;
    assert_eq!(g.phase, RunPhase::Idle);
    assert!(g.messages.is_empty());
    assert!(g.terminal_lines.is_empty());
    assert!(g.config.slots.iter().all(|s| s.usage.total_tokens == 0));
    let _ = std::fs::remove_dir_all(&temp);
}
