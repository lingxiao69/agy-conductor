use crate::agy::terminal::{launch_single_cli, launch_split_team_cli, TerminalLaunchOptions};
use crate::billing::SlotUsageStats;
use crate::ipc::dispatcher::{emit_ui, toast};
use crate::ipc::state::SharedAppState;
use crate::ipc::{AppCustomEvent, UiEvent};
use crate::orchestrator::engine::EngineControl;
use crate::orchestrator::types::{AgentSlot, SlotRunState};
use crate::storage::brain_health::MAX_HEALTHY_REUSE_CONTEXT_TOKENS;
use crate::storage::store::AppStorage;
use crate::storage::workspace_sessions::{
    discover_workspace_role_sessions, normalize_workspace_key, resolve_reused_sessions_for_slots,
    save_room_role_sessions, upsert_workspace_role_session, upsert_workspace_role_sessions,
    WorkspaceRoleSession, SESSION_SOURCE_CONDUCTOR,
};
use chrono::Local;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tao::event_loop::EventLoopProxy;

#[derive(Debug, Clone, Default)]
pub struct SessionReuseOutcome {
    pub reused: Vec<(String, String, u64)>,
    pub skipped_unhealthy: Vec<(String, String)>,
}

pub async fn handle_discover_workspace_sessions(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    workspace: String,
) {
    let trimmed = workspace.trim().to_string();
    let (storage, reuse_role_sessions) = {
        let guard = state.lock().await;
        (guard.storage.clone(), guard.config.reuse_role_sessions)
    };

    if trimmed.is_empty() {
        {
            state.lock().await.workspace_sessions.clear();
        }
        emit_ui(
            &proxy,
            UiEvent::WorkspaceSessionsDiscovered {
                workspace: String::new(),
                reuse_role_sessions,
                sessions: Vec::new(),
            },
        );
        return;
    }

    let ws_clone = trimmed.clone();
    let sessions = tokio::task::spawn_blocking(move || {
        let found = discover_workspace_role_sessions(&storage, &ws_clone, None);
        if !found.is_empty() {
            let _ = upsert_workspace_role_sessions(&storage, &found);
        }
        found
    })
    .await
    .unwrap_or_default();

    let latest_reuse = {
        let mut guard = state.lock().await;
        if normalize_workspace_key(&guard.config.last_workspace) == normalize_workspace_key(&trimmed)
            || guard.config.last_workspace.trim().is_empty()
        {
            guard.workspace_sessions = sessions.clone();
        }
        guard.config.reuse_role_sessions
    };

    emit_ui(
        &proxy,
        UiEvent::WorkspaceSessionsDiscovered {
            workspace: trimmed,
            reuse_role_sessions: latest_reuse,
            sessions,
        },
    );
}

pub async fn handle_set_reuse_role_sessions(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    reuse: bool,
) {
    let (workspace, sessions) = {
        let mut guard = state.lock().await;
        guard.config.reuse_role_sessions = reuse;
        guard.save_persisted_config();
        (
            guard.config.last_workspace.clone(),
            guard.workspace_sessions.clone(),
        )
    };
    emit_ui(
        &proxy,
        UiEvent::WorkspaceSessionsDiscovered {
            workspace,
            reuse_role_sessions: reuse,
            sessions,
        },
    );
}

pub async fn handle_clear_terminal_log(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
) {
    {
        state.lock().await.terminal_lines.clear();
    }
    emit_ui(&proxy, UiEvent::TerminalLogCleared);
}

/// 根据启动页“沿用历史角色会话”开关与显式选择映射，为槽位注入历史 `conversation_id`，
/// 并返回详细的沿用明细与被自动跳过的不健康旧会话原因。
pub fn apply_session_reuse_for_slots_detailed(
    slots: &mut [AgentSlot],
    discovered_sessions: &[WorkspaceRoleSession],
    reuse_role_sessions: bool,
    selected_sessions: &HashMap<String, String>,
) -> SessionReuseOutcome {
    for slot in slots.iter_mut() {
        slot.state = SlotRunState::Idle;
        slot.current_activity = None;
        slot.consecutive_failures = 0;
        slot.conversation_id = None;
        slot.session_reused = false;
        slot.usage = SlotUsageStats::for_model(&slot.model_id);
    }

    if !reuse_role_sessions {
        return SessionReuseOutcome::default();
    }

    let matched = resolve_reused_sessions_for_slots(slots, discovered_sessions, selected_sessions);
    let mut outcome = SessionReuseOutcome::default();

    for slot in slots.iter_mut() {
        if let Some(found) = matched.get(&slot.slot_id) {
            slot.conversation_id = Some(found.conversation_id.clone());
            slot.session_reused = true;
            outcome.reused.push((
                slot.display_name.clone(),
                found.conversation_id.clone(),
                found.context_tokens,
            ));
            continue;
        }

        if !slot.enabled {
            continue;
        }
        let explicitly_new = selected_sessions
            .get(&slot.slot_id)
            .map(|v| {
                let t = v.trim();
                t.eq_ignore_ascii_case("__new__") || t.eq_ignore_ascii_case("none")
            })
            .unwrap_or(false);
        if explicitly_new {
            continue;
        }

        if let Some(bad) = discovered_sessions.iter().find(|s| {
            !s.healthy_for_reuse
                && (s.slot_id == slot.slot_id
                    || s.role == slot.role
                    || s.alias.eq_ignore_ascii_case(&slot.alias))
        }) {
            let reason = bad
                .unhealthy_reason
                .clone()
                .unwrap_or_else(|| "上下文过大或末尾异常".to_string());
            outcome
                .skipped_unhealthy
                .push((slot.display_name.clone(), reason));
        }
    }
    outcome
}

/// 兼容旧调用方的简化封装：返回实际沿用成功的 `(slot_display_name, conversation_id)` 列表。
pub fn apply_session_reuse_for_slots(
    slots: &mut [AgentSlot],
    discovered_sessions: &[WorkspaceRoleSession],
    reuse_role_sessions: bool,
    selected_sessions: &HashMap<String, String>,
) -> Vec<(String, String)> {
    apply_session_reuse_for_slots_detailed(
        slots,
        discovered_sessions,
        reuse_role_sessions,
        selected_sessions,
    )
    .reused
    .into_iter()
    .map(|(name, cid, _)| (name, cid))
    .collect()
}

fn build_slot_session_record(
    ws: &str,
    ws_key: &str,
    room_id: Option<&str>,
    goal: &str,
    now: &str,
    slot: &AgentSlot,
) -> Option<WorkspaceRoleSession> {
    let cid = slot
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())?;
    let ctx = slot.usage.peak_context_tokens;
    let healthy = ctx <= MAX_HEALTHY_REUSE_CONTEXT_TOKENS;
    let unhealthy_reason = if healthy {
        None
    } else {
        Some(format!("上下文体积已达 {}k，建议开启新会话", ctx / 1000))
    };

    Some(WorkspaceRoleSession {
        workspace: ws.to_string(),
        workspace_key: ws_key.to_string(),
        slot_id: slot.slot_id.clone(),
        alias: slot.alias.clone(),
        display_name: slot.display_name.clone(),
        role: slot.role,
        model_id: slot.model_id.clone(),
        conversation_id: cid.to_string(),
        last_goal: goal.trim().to_string(),
        room_id: room_id.map(str::to_string),
        updated_at: now.to_string(),
        source: SESSION_SOURCE_CONDUCTOR.to_string(),
        context_tokens: ctx,
        healthy_for_reuse: healthy,
        unhealthy_reason,
    })
}

/// 启动编排或批量绑定会话时，将所有已绑定 `conversation_id` 的槽位立即持久化至
/// 全局 `workspace_sessions.json` 与当前 `rooms/<id>/sessions.json`。
pub fn persist_all_active_slot_sessions(
    storage: &AppStorage,
    workspace: &str,
    room_id: Option<&str>,
    room_dir: Option<&Path>,
    goal: &str,
    slots: &[AgentSlot],
) {
    let ws = workspace.trim();
    if ws.is_empty() {
        return;
    }
    let ws_key = normalize_workspace_key(ws);
    let now = Local::now().to_rfc3339();
    let all_active: Vec<WorkspaceRoleSession> = slots
        .iter()
        .filter_map(|s| build_slot_session_record(ws, &ws_key, room_id, goal, &now, s))
        .collect();
    if all_active.is_empty() {
        return;
    }
    let _ = upsert_workspace_role_sessions(storage, &all_active);
    if let Some(dir) = room_dir {
        let _ = save_room_role_sessions(dir, &all_active);
    }
}

/// 在运行中实时持久化槽位最新会话 ID 至全局 `workspace_sessions.json` 与当前 `rooms/<id>/sessions.json`。
pub fn persist_slot_session_update(
    storage: &AppStorage,
    workspace: &str,
    room_id: Option<&str>,
    room_dir: Option<&Path>,
    goal: &str,
    slots: &[AgentSlot],
    changed_slot_id: &str,
) {
    let ws = workspace.trim();
    if ws.is_empty() {
        return;
    }
    let ws_key = normalize_workspace_key(ws);
    let now = Local::now().to_rfc3339();

    if let Some(slot) = slots.iter().find(|s| s.slot_id == changed_slot_id) {
        if let Some(record) = build_slot_session_record(ws, &ws_key, room_id, goal, &now, slot) {
            let _ = upsert_workspace_role_session(storage, &record);
        }
    }

    if let Some(dir) = room_dir {
        let all_active: Vec<WorkspaceRoleSession> = slots
            .iter()
            .filter_map(|s| build_slot_session_record(ws, &ws_key, room_id, goal, &now, s))
            .collect();
        if !all_active.is_empty() {
            let _ = save_room_role_sessions(dir, &all_active);
        }
    }
}

pub async fn handle_open_terminal(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    slot_id: Option<String>,
    attach_conversation: bool,
    split_all: bool,
    mount_mcp: bool,
    watch_live: bool,
) {
    let effective_attach = attach_conversation && !watch_live;
    let effective_mcp = mount_mcp && !watch_live;

    let (runtime, workspace, room_dir, slots, target_slot, engine_tx) = {
        let mut guard = state.lock().await;
        let Some(rt) = guard.runtime.clone() else {
            toast(&proxy, "error", "尚未检测到 agy 运行时，无法拉起终端。");
            return;
        };
        let ws = PathBuf::from(guard.config.last_workspace.trim());
        let room_dir = if let Some(existing) = guard.current_room_dir.clone() {
            Some(existing)
        } else if watch_live || effective_mcp {
            let id = format!("cli-{}", uuid::Uuid::new_v4().simple());
            let created = guard.storage.create_room_dir(&id).ok();
            guard.current_room_dir = created.clone();
            created
        } else {
            None
        };
        let target = slot_id
            .as_deref()
            .and_then(|id| guard.config.slots.iter().find(|s| s.slot_id == id))
            .cloned();

        if effective_attach {
            if let Some(ref s) = target {
                if s.state == SlotRunState::Working {
                    toast(
                        &proxy,
                        "warning",
                        format!(
                            "{} 正在执行后台轮次，请等待本轮完成或暂停后再接管会话（可先点「追踪实时进程」围观）。",
                            s.display_name
                        ),
                    );
                    return;
                }
                if let Some(slot_mut) = guard
                    .config
                    .slots
                    .iter_mut()
                    .find(|item| item.slot_id == s.slot_id)
                {
                    slot_mut.state = SlotRunState::ExternalCliAttached;
                    slot_mut.current_activity = Some("已由外部 CLI 接管".to_string());
                }
            }
        }

        (
            rt,
            ws,
            room_dir,
            guard.config.slots.clone(),
            target,
            guard.active_engine_tx.clone(),
        )
    };

    let opts = TerminalLaunchOptions {
        workspace_dir: workspace,
        room_dir,
        mount_mcp: effective_mcp,
        attach_conversation: effective_attach,
        watch_live,
    };

    let result = if split_all {
        launch_split_team_cli(&runtime, &slots, &opts)
    } else {
        launch_single_cli(&runtime, target_slot.as_ref(), &opts)
    };

    match result {
        Ok(msg) => {
            if effective_attach {
                if let Some(s) = target_slot {
                    if let Some(tx) = engine_tx {
                        let _ = tx.send(EngineControl::SetSlotCliAttached {
                            slot_id: s.slot_id.clone(),
                        });
                    }
                    emit_ui(
                        &proxy,
                        UiEvent::SlotStateChanged {
                            slot_id: s.slot_id,
                            state: SlotRunState::ExternalCliAttached,
                            activity: Some("已由外部 CLI 接管".to_string()),
                            conversation_id: s.conversation_id,
                            session_reused: s.session_reused,
                        },
                    );
                }
            }
            toast(&proxy, "success", msg);
        }
        Err(err) => {
            if effective_attach {
                if let Some(s) = target_slot {
                    let mut guard = state.lock().await;
                    if let Some(slot_mut) = guard
                        .config
                        .slots
                        .iter_mut()
                        .find(|item| item.slot_id == s.slot_id)
                    {
                        slot_mut.state = SlotRunState::Idle;
                        slot_mut.current_activity = None;
                    }
                }
            }
            toast(&proxy, "error", err);
        }
    }
}
