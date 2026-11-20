use crate::agy::resolver::run_doctor;
use crate::agy::terminal::open_folder_in_explorer;
use crate::ipc::cli_hub_actions::{
    handle_launch_provider_external_cli, handle_refresh_cli_providers, handle_select_cli_provider,
};
use crate::ipc::env_actions::{handle_run_agy_update, refresh_agy_environment};
use crate::ipc::history_preset_actions::{
    handle_apply_roster_preset, handle_delete_history_room, handle_delete_roster_preset,
    handle_export_history_markdown, handle_list_history_rooms, handle_load_history_room,
    handle_save_roster_preset,
};
use crate::ipc::run_actions::start_orchestrator_run;
use crate::ipc::session_terminal_actions::{
    handle_clear_terminal_log, handle_discover_workspace_sessions, handle_open_terminal,
    handle_set_reuse_role_sessions,
};
use crate::ipc::state::SharedAppState;
use crate::ipc::usage_actions::{handle_export_report_markdown, handle_load_cli_usage_stats};
use crate::ipc::{AppCustomEvent, UiCommand, UiEvent};
use crate::orchestrator::engine::EngineControl;
use crate::orchestrator::types::SlotRunState;
use crate::usage_stats::UsageRange;
use std::path::PathBuf;
use tao::event_loop::EventLoopProxy;

pub fn emit_ui(proxy: &EventLoopProxy<AppCustomEvent>, event: UiEvent) {
    let _ = proxy.send_event(AppCustomEvent::DispatchUiEvent(event));
}

pub fn toast(proxy: &EventLoopProxy<AppCustomEvent>, level: &str, message: impl Into<String>) {
    emit_ui(
        proxy,
        UiEvent::SystemToast {
            level: level.to_string(),
            message: message.into(),
        },
    );
}

pub async fn dispatch_ui_command(
    raw_json: String,
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
) {
    let cmd: UiCommand = match serde_json::from_str(&raw_json) {
        Ok(c) => c,
        Err(e) => {
            toast(&proxy, "error", format!("前端指令解析失败: {e}"));
            return;
        }
    };

    match cmd {
        UiCommand::InitApp => {
            let (snap, last_ws) = {
                let guard = state.lock().await;
                (guard.snapshot(), guard.config.last_workspace.clone())
            };
            emit_ui(&proxy, UiEvent::StateSnapshot(snap));
            if !last_ws.trim().is_empty() {
                let st = state.clone();
                let px = proxy.clone();
                tokio::spawn(async move {
                    handle_discover_workspace_sessions(st, px, last_ws).await;
                });
            }
            {
                let st = state.clone();
                let px = proxy.clone();
                tokio::spawn(async move {
                    handle_refresh_cli_providers(st, px, false).await;
                });
            }
            {
                let st = state.clone();
                let px = proxy.clone();
                tokio::spawn(async move {
                    handle_load_cli_usage_stats(st, px, UsageRange::Days7, false).await;
                });
            }
            refresh_agy_environment(state, proxy, false).await;
        }
        UiCommand::RefreshModels => {
            refresh_agy_environment(state, proxy, true).await;
        }
        UiCommand::RunDoctor => {
            let custom = { state.lock().await.config.custom_agy_path.clone() };
            let report = run_doctor(custom.as_deref()).await;
            {
                state.lock().await.doctor = Some(report.clone());
            }
            emit_ui(&proxy, UiEvent::DoctorUpdated(report));
        }
        UiCommand::PickWorkspaceFolder => {
            let initial = { state.lock().await.config.last_workspace.clone() };
            let picked = tokio::task::spawn_blocking(move || {
                let mut dialog = rfd::FileDialog::new().set_title("选择目标项目文件夹");
                if !initial.is_empty() && PathBuf::from(&initial).is_dir() {
                    dialog = dialog.set_directory(&initial);
                }
                dialog.pick_folder()
            })
            .await
            .ok()
            .flatten();

            if let Some(folder) = picked {
                let path_str = folder.to_string_lossy().to_string();
                {
                    let mut guard = state.lock().await;
                    guard.config.last_workspace = path_str.clone();
                    guard.save_persisted_config();
                }
                emit_ui(
                    &proxy,
                    UiEvent::WorkspaceSelected {
                        workspace: path_str.clone(),
                    },
                );
                handle_discover_workspace_sessions(state, proxy, path_str).await;
            }
        }
        UiCommand::SetWorkspaceFolder { workspace } => {
            let cleaned = workspace.trim().to_string();
            {
                let mut guard = state.lock().await;
                guard.config.last_workspace = cleaned.clone();
                guard.save_persisted_config();
            }
            emit_ui(
                &proxy,
                UiEvent::WorkspaceSelected {
                    workspace: cleaned.clone(),
                },
            );
            handle_discover_workspace_sessions(state, proxy, cleaned).await;
        }
        UiCommand::DiscoverWorkspaceSessions { workspace } => {
            handle_discover_workspace_sessions(state, proxy, workspace).await;
        }
        UiCommand::SetReuseRoleSessions { reuse } => {
            handle_set_reuse_role_sessions(state, proxy, reuse).await;
        }
        UiCommand::ClearTerminalLog => {
            handle_clear_terminal_log(state, proxy).await;
        }
        UiCommand::TogglePreferGateway { prefer_gateway } => {
            let snap = {
                let mut guard = state.lock().await;
                guard.config.prefer_gateway_models = prefer_gateway;
                guard.save_persisted_config();
                guard.snapshot()
            };
            emit_ui(&proxy, UiEvent::StateSnapshot(snap));
        }
        UiCommand::ApplyDefaultRoster => {
            let snap = {
                let mut guard = state.lock().await;
                guard.apply_default_roster();
                guard.snapshot()
            };
            emit_ui(&proxy, UiEvent::StateSnapshot(snap));
            toast(&proxy, "success", "已按本机探测模型重置推荐编队");
        }
        UiCommand::SaveRoster {
            slots,
            dispatch_mode,
        } => {
            let (engine_tx, normalized_slots, snap) = {
                let mut guard = state.lock().await;
                guard.set_slots_and_mode(slots, dispatch_mode);
                (
                    guard.active_engine_tx.clone(),
                    guard.config.slots.clone(),
                    guard.snapshot(),
                )
            };
            if let Some(tx) = engine_tx {
                let _ = tx.send(EngineControl::UpdateDispatchMode(dispatch_mode));
                let _ = tx.send(EngineControl::UpdateSlots(normalized_slots));
            }
            emit_ui(&proxy, UiEvent::StateSnapshot(snap));
        }
        UiCommand::SaveRosterPreset {
            name,
            slots,
            dispatch_mode,
        } => {
            handle_save_roster_preset(state, proxy, name, slots, dispatch_mode).await;
        }
        UiCommand::ApplyRosterPreset { preset_id } => {
            handle_apply_roster_preset(state, proxy, preset_id).await;
        }
        UiCommand::DeleteRosterPreset { preset_id } => {
            handle_delete_roster_preset(state, proxy, preset_id).await;
        }
        UiCommand::ListHistoryRooms => {
            handle_list_history_rooms(state, proxy).await;
        }
        UiCommand::LoadHistoryRoom { room_id } => {
            handle_load_history_room(state, proxy, room_id).await;
        }
        UiCommand::DeleteHistoryRoom { room_id } => {
            handle_delete_history_room(state, proxy, room_id).await;
        }
        UiCommand::ExportHistoryMarkdown { room_id } => {
            handle_export_history_markdown(state, proxy, room_id).await;
        }
        UiCommand::StartGoal {
            workspace,
            goal,
            permission_mode,
            enable_sandbox,
            max_rework_rounds,
            reuse_role_sessions,
            selected_sessions,
        } => {
            start_orchestrator_run(
                state,
                proxy,
                workspace,
                goal,
                permission_mode,
                enable_sandbox,
                max_rework_rounds,
                reuse_role_sessions,
                selected_sessions,
            )
            .await;
        }
        UiCommand::PauseRun => {
            if let Some(tx) = { state.lock().await.active_engine_tx.clone() } {
                let _ = tx.send(EngineControl::Pause);
            }
        }
        UiCommand::ResumeRun => {
            if let Some(tx) = { state.lock().await.active_engine_tx.clone() } {
                let _ = tx.send(EngineControl::Resume);
            }
        }
        UiCommand::StopRun => {
            if let Some(tx) = { state.lock().await.active_engine_tx.clone() } {
                let _ = tx.send(EngineControl::Stop);
            }
        }
        UiCommand::ResetRun => {
            let (snap, last_ws) = {
                let mut guard = state.lock().await;
                guard.reset_run_state();
                (guard.snapshot(), guard.config.last_workspace.clone())
            };
            emit_ui(&proxy, UiEvent::StateSnapshot(snap));
            if !last_ws.trim().is_empty() {
                handle_discover_workspace_sessions(state, proxy, last_ws).await;
            }
        }
        UiCommand::PostUserMessage { text } => {
            let tx = { state.lock().await.active_engine_tx.clone() };
            if let Some(tx) = tx {
                let _ = tx.send(EngineControl::PostUserMessage(text));
            } else {
                toast(&proxy, "info", "当前尚未启动自动研发会话，请先设定长期目标并启动。");
            }
        }
        UiCommand::OpenTerminal {
            slot_id,
            attach_conversation,
            split_all,
            mount_mcp,
            watch_live,
        } => {
            handle_open_terminal(
                state,
                proxy,
                slot_id,
                attach_conversation,
                split_all,
                mount_mcp,
                watch_live,
            )
            .await;
        }
        UiCommand::ReleaseTerminalAttach { slot_id } => {
            let (tx, updated_slot) = {
                let mut guard = state.lock().await;
                let mut found = None;
                if let Some(s) = guard
                    .config
                    .slots
                    .iter_mut()
                    .find(|s| s.slot_id == slot_id)
                {
                    if s.state == SlotRunState::ExternalCliAttached {
                        s.state = SlotRunState::Idle;
                        s.current_activity = None;
                        found = Some(s.clone());
                    }
                }
                (guard.active_engine_tx.clone(), found)
            };
            if let Some(s) = updated_slot {
                if let Some(tx) = tx {
                    let _ = tx.send(EngineControl::ReleaseSlotCliAttached {
                        slot_id: slot_id.clone(),
                    });
                }
                emit_ui(
                    &proxy,
                    UiEvent::SlotStateChanged {
                        slot_id: s.slot_id,
                        state: s.state,
                        activity: s.current_activity,
                        conversation_id: s.conversation_id,
                        session_reused: s.session_reused,
                    },
                );
            }
        }
        UiCommand::RunAgyUpdate => {
            handle_run_agy_update(state, proxy).await;
        }
        UiCommand::OpenWorkspaceExplorer => {
            let ws = { state.lock().await.config.last_workspace.clone() };
            if ws.trim().is_empty() {
                toast(&proxy, "warning", "请先选择项目工作目录");
            } else if let Err(e) = open_folder_in_explorer(&PathBuf::from(&ws)) {
                toast(&proxy, "error", e);
            }
        }
        UiCommand::SelectCliProvider { provider } => {
            handle_select_cli_provider(state, proxy, provider).await;
        }
        UiCommand::RefreshCliProviders => {
            handle_refresh_cli_providers(state, proxy, true).await;
        }
        UiCommand::LaunchProviderExternalCli { provider } => {
            handle_launch_provider_external_cli(state, proxy, provider).await;
        }
        UiCommand::LoadCliUsageStats {
            range,
            force_refresh,
        } => {
            handle_load_cli_usage_stats(state, proxy, range, force_refresh).await;
        }
        UiCommand::ExportReportMarkdown { title, markdown } => {
            handle_export_report_markdown(state, proxy, title, markdown).await;
        }
    }
}
