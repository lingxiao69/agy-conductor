use crate::agy::live_log::{LiveLogSink, LiveStreamKind, LiveTerminalLine};
use crate::agy::runner::RealAgyRunner;
use crate::ipc::dispatcher::{emit_ui, toast};
use crate::ipc::run_events::{apply_live_line_for_generation, spawn_engine_event_relay};
use crate::ipc::session_terminal_actions::{
    apply_session_reuse_for_slots_detailed, persist_all_active_slot_sessions, SessionReuseOutcome,
};
use crate::ipc::state::SharedAppState;
use crate::ipc::{AppCustomEvent, UiEvent};
use crate::orchestrator::engine::{EngineConfig, OrchestratorEngine};
use crate::orchestrator::engine_turns::make_system_message;
use crate::orchestrator::schemas::write_role_schemas;
use crate::orchestrator::types::{
    GroupChatMessage, MessageBadge, PermissionMode, RoleKind, RunPhase,
};
use crate::storage::store::AppStorage;
use crate::storage::workspace_sessions::{
    discover_workspace_role_sessions, upsert_workspace_role_sessions,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tao::event_loop::EventLoopProxy;
use tokio::sync::mpsc;

#[allow(clippy::too_many_arguments)]
pub async fn start_orchestrator_run(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    workspace: String,
    goal: String,
    permission_mode: PermissionMode,
    enable_sandbox: bool,
    max_rework_rounds: u8,
    reuse_role_sessions: bool,
    selected_sessions: HashMap<String, String>,
) {
    let ws_path = PathBuf::from(workspace.trim());
    if workspace.trim().is_empty() || !ws_path.is_dir() {
        toast(&proxy, "error", "请先选择一个有效的本地项目目录。");
        return;
    }
    if goal.trim().is_empty() {
        toast(&proxy, "error", "请输入清晰的长期研发目标后再启动。");
        return;
    }

    let discovered_sessions = if reuse_role_sessions {
        let storage = { state.lock().await.storage.clone() };
        let ws_for_scan = workspace.trim().to_string();
        tokio::task::spawn_blocking(move || {
            let found = discover_workspace_role_sessions(&storage, &ws_for_scan, None);
            if !found.is_empty() {
                let _ = upsert_workspace_role_sessions(&storage, &found);
            }
            found
        })
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };

    let setup = {
        let mut guard = state.lock().await;
        if guard.is_run_active() {
            toast(&proxy, "warning", "已有研发协同任务正在运行中。");
            return;
        }
        let Some(runtime) = guard.runtime.clone() else {
            toast(&proxy, "error", "尚未检测到可用的 agy 运行时，请先点击刷新模型。");
            return;
        };

        let has_arch = guard
            .config
            .slots
            .iter()
            .any(|s| s.enabled && s.role == RoleKind::Architect);
        let has_coder = guard
            .config
            .slots
            .iter()
            .any(|s| s.enabled && s.role == RoleKind::Coder);
        let has_rev = guard
            .config
            .slots
            .iter()
            .any(|s| s.enabled && s.role == RoleKind::Reviewer);
        if !has_arch || !has_coder || !has_rev {
            toast(
                &proxy,
                "error",
                "编队中必须至少启用 1 位总架构师、1 位编码程序员与 1 位代码审查员。",
            );
            return;
        }

        guard.apply_run_preferences(
            workspace.clone(),
            goal.clone(),
            permission_mode,
            enable_sandbox,
            max_rework_rounds,
            reuse_role_sessions,
        );

        let room_id = format!("room-{}", uuid::Uuid::new_v4().simple());
        let room_dir = match guard.storage.create_room_dir(&room_id) {
            Ok(d) => d,
            Err(e) => {
                toast(&proxy, "error", e);
                return;
            }
        };
        let schemas = match write_role_schemas(&guard.storage.schemas_dir()) {
            Ok(s) => s,
            Err(e) => {
                toast(&proxy, "error", e);
                return;
            }
        };

        let reuse_outcome = apply_session_reuse_for_slots_detailed(
            &mut guard.config.slots,
            &discovered_sessions,
            reuse_role_sessions,
            &selected_sessions,
        );
        if reuse_role_sessions {
            guard.workspace_sessions = discovered_sessions;
        }
        guard.save_persisted_config();

        guard.run_generation = guard.run_generation.wrapping_add(1);
        let run_gen = guard.run_generation;

        guard.tasks.clear();
        guard.messages.clear();
        guard.terminal_lines.clear();
        guard.active_blocker = None;
        guard.completion = None;
        guard.current_room_id = Some(room_id.clone());
        guard.current_room_dir = Some(room_dir.clone());
        guard.phase = RunPhase::Planning;

        let startup_msg = build_startup_session_chat_message(&reuse_outcome, reuse_role_sessions);
        guard.messages.push(startup_msg.clone());
        let _ = AppStorage::append_board_message(&room_dir, &startup_msg);

        let (live_tx, live_rx) = mpsc::unbounded_channel::<LiveTerminalLine>();
        let live_sink = Arc::new(LiveLogSink::new(
            room_dir.clone(),
            Some(live_tx),
            &guard.config.slots,
        ));
        guard.live_log_sink = Some(Arc::clone(&live_sink));

        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
        guard.active_engine_tx = Some(ctrl_tx);

        let cfg = EngineConfig {
            room_id: room_id.clone(),
            room_dir: room_dir.clone(),
            workspace_dir: ws_path,
            goal: goal.clone(),
            permission_mode,
            enable_sandbox,
            max_rework_rounds: max_rework_rounds.clamp(1, 20),
            turn_timeout_secs: 1800,
            dispatch_mode: guard.config.dispatch_mode,
            slots: guard.config.slots.clone(),
            schemas,
        };
        let storage_for_persist = guard.storage.clone();
        let job_guard = guard.job_guard.clone();
        let snap = guard.snapshot();
        (
            runtime,
            job_guard,
            storage_for_persist,
            cfg,
            ctrl_rx,
            snap,
            live_sink,
            live_rx,
            reuse_outcome,
            run_gen,
        )
    };

    let (
        runtime,
        job_guard,
        storage_for_persist,
        cfg,
        ctrl_rx,
        snap,
        live_sink,
        mut live_rx,
        reuse_outcome,
        run_gen,
    ) = setup;
    emit_ui(&proxy, UiEvent::StateSnapshot(snap));

    // 启动瞬间即落盘所有预填沿用会话，彻底修复“沿用旧会话不触发 workspace_sessions.json 落盘”的缺陷。
    {
        let ws_str = workspace.clone();
        let rid = cfg.room_id.clone();
        let rdir = cfg.room_dir.clone();
        let goal_str = goal.clone();
        let slots_snap = cfg.slots.clone();
        tokio::task::spawn_blocking(move || {
            persist_all_active_slot_sessions(
                &storage_for_persist,
                &ws_str,
                Some(&rid),
                Some(&rdir),
                &goal_str,
                &slots_snap,
            );
        });
    }

    let state_for_logs = Arc::clone(&state);
    let proxy_for_logs = proxy.clone();
    tokio::spawn(async move {
        while let Some(line) = live_rx.recv().await {
            if !apply_live_line_for_generation(&state_for_logs, run_gen, line.clone()).await {
                break;
            }
            emit_ui(&proxy_for_logs, UiEvent::TerminalLineAppended(line));
        }
    });

    emit_startup_live_log_and_toast(&live_sink, &proxy, &reuse_outcome);

    let (evt_tx, evt_rx) = mpsc::unbounded_channel();
    let runner = Arc::new(RealAgyRunner::with_live_log(
        runtime,
        job_guard,
        Some(Arc::clone(&live_sink)),
    ));
    let engine = OrchestratorEngine::new(cfg, runner, evt_tx, ctrl_rx);

    spawn_engine_event_relay(Arc::clone(&state), proxy, run_gen, evt_rx);

    tokio::spawn(async move {
        engine.run().await;
    });
}

fn build_startup_session_chat_message(
    outcome: &SessionReuseOutcome,
    reuse_enabled: bool,
) -> GroupChatMessage {
    let mut parts: Vec<String> = Vec::new();
    if !outcome.reused.is_empty() {
        let items: Vec<String> = outcome
            .reused
            .iter()
            .map(|(name, cid, ctx)| {
                let short: String = cid.chars().take(6).collect();
                if *ctx > 0 {
                    format!("{name} (#{short} · {}k)", ctx / 1000)
                } else {
                    format!("{name} (#{short})")
                }
            })
            .collect();
        parts.push(format!(
            "已为 {} 位角色沿用历史项目记忆：{}",
            outcome.reused.len(),
            items.join("、")
        ));
    }
    if !outcome.skipped_unhealthy.is_empty() {
        let skipped: Vec<String> = outcome
            .skipped_unhealthy
            .iter()
            .map(|(name, reason)| format!("{name}（{reason}）"))
            .collect();
        parts.push(format!(
            "以下角色因历史会话体积过大或末尾异常，已自动改为开启全新轻量会话以防卡死：{}",
            skipped.join("；")
        ));
    }
    if parts.is_empty() {
        if reuse_enabled {
            parts.push("当前项目目录暂无可健康沿用的历史会话，已为全体角色开启全新轻量上下文。".to_string());
        } else {
            parts.push("已按配置为全体角色开启全新项目会话上下文。".to_string());
        }
    }

    make_system_message(parts.join("\n"), MessageBadge::Info, None)
}

fn emit_startup_live_log_and_toast(
    live_sink: &LiveLogSink,
    proxy: &EventLoopProxy<AppCustomEvent>,
    outcome: &SessionReuseOutcome,
) {
    if !outcome.reused.is_empty() {
        let names = outcome
            .reused
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect::<Vec<_>>()
            .join("、");
        live_sink.append_line(
            "system",
            "system",
            "轻舟协调器",
            LiveStreamKind::System,
            format!("已沿用 {} 位角色的历史项目记忆会话：{names}", outcome.reused.len()),
        );
        toast(
            proxy,
            "info",
            format!("已沿用 {} 位角色的历史项目会话记忆，免重新摸底。", outcome.reused.len()),
        );
    } else if !outcome.skipped_unhealthy.is_empty() {
        live_sink.append_line(
            "system",
            "system",
            "轻舟协调器",
            LiveStreamKind::System,
            format!(
                "已自动跳过 {} 个过载/异常旧会话并开启全新轻量上下文，实时进程日志追踪已就绪。",
                outcome.skipped_unhealthy.len()
            ),
        );
    } else {
        live_sink.append_line(
            "system",
            "system",
            "轻舟协调器",
            LiveStreamKind::System,
            "已启动全新角色会话上下文，实时进程日志追踪已就绪。",
        );
    }
}
