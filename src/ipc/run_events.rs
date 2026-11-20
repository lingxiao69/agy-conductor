use crate::agy::live_log::LiveTerminalLine;
use crate::ipc::dispatcher::{emit_ui, toast};
use crate::ipc::session_terminal_actions::persist_slot_session_update;
use crate::ipc::state::SharedAppState;
use crate::ipc::{AppCustomEvent, UiEvent};
use crate::orchestrator::engine::EngineEvent;
use crate::orchestrator::engine_turns::format_popup_summary;
use crate::orchestrator::types::{CompletionSummary, RunPhase, SlotRunState, UserBlockerAlert};
use crate::win_guard::notifier::{notify_blocker_needs_user, notify_project_completed};
use std::collections::HashSet;
use tao::event_loop::EventLoopProxy;
use tokio::sync::mpsc;

pub enum RelayDispatch {
    Ui(UiEvent),
    Blocker {
        hwnd: Option<isize>,
        alert: UserBlockerAlert,
    },
    Completed {
        hwnd: Option<isize>,
        summary: CompletionSummary,
    },
    Toast {
        level: String,
        message: String,
    },
}

/// 校验运行代际后再写入实时终端行；若已过期返回 `false`，调用方应立即退出转发循环。
pub async fn apply_live_line_for_generation(
    state: &SharedAppState,
    run_gen: u64,
    line: LiveTerminalLine,
) -> bool {
    let mut guard = state.lock().await;
    if guard.run_generation != run_gen {
        return false;
    }
    guard.push_terminal_line(line);
    true
}

/// 校验运行代际后将引擎事件同步至 `AppCoreState`；
/// 一旦检测到 `run_generation` 不匹配（发生过 `ResetRun` 或新一轮 `StartGoal`），
/// 立即返回 `None` 丢弃事件，防止旧引擎延迟消息污染重置后或新开局的状态。
pub async fn apply_engine_event_for_generation(
    state: &SharedAppState,
    run_gen: u64,
    evt: EngineEvent,
    persisted_slots: &mut HashSet<String>,
) -> Option<RelayDispatch> {
    match evt {
        EngineEvent::PhaseChanged(phase) => {
            let mut guard = state.lock().await;
            if guard.run_generation != run_gen {
                return None;
            }
            guard.phase = phase;
            if phase != RunPhase::WaitingUser {
                guard.active_blocker = None;
            }
            if matches!(
                phase,
                RunPhase::Completed | RunPhase::Stopped | RunPhase::Failed
            ) {
                guard.active_engine_tx = None;
                guard.live_log_sink = None;
            }
            Some(RelayDispatch::Ui(UiEvent::PhaseChanged { phase }))
        }
        EngineEvent::SlotStateChanged {
            slot_id,
            state: st,
            activity,
            conversation_id,
            session_reused,
        } => {
            let persist_ctx = {
                let mut guard = state.lock().await;
                if guard.run_generation != run_gen {
                    return None;
                }
                let mut cid_updated = false;
                let mut has_non_empty_cid = false;
                if let Some(slot) = guard
                    .config
                    .slots
                    .iter_mut()
                    .find(|s| s.slot_id == slot_id)
                {
                    slot.state = st;
                    slot.session_reused = session_reused;
                    slot.current_activity = activity.clone();
                    if let Some(ref cid) = conversation_id {
                        if !cid.trim().is_empty() {
                            if slot.conversation_id.as_deref() != Some(cid.as_str()) {
                                slot.conversation_id = Some(cid.clone());
                                cid_updated = true;
                            }
                            has_non_empty_cid = true;
                        }
                    } else if slot
                        .conversation_id
                        .as_deref()
                        .map(|c| !c.trim().is_empty())
                        .unwrap_or(false)
                    {
                        has_non_empty_cid = true;
                    }
                }
                let should_persist = cid_updated
                    || (has_non_empty_cid
                        && (st == SlotRunState::Idle || !persisted_slots.contains(&slot_id)));
                if cid_updated {
                    guard.save_persisted_config();
                }
                if should_persist {
                    persisted_slots.insert(slot_id.clone());
                    Some((
                        guard.storage.clone(),
                        guard.config.last_workspace.clone(),
                        guard.current_room_id.clone(),
                        guard.current_room_dir.clone(),
                        guard.goal.clone(),
                        guard.config.slots.clone(),
                        slot_id.clone(),
                    ))
                } else {
                    None
                }
            };
            if let Some((storage, ws, rid, rdir, goal_txt, slots_snap, sid)) = persist_ctx {
                tokio::task::spawn_blocking(move || {
                    persist_slot_session_update(
                        &storage,
                        &ws,
                        rid.as_deref(),
                        rdir.as_deref(),
                        &goal_txt,
                        &slots_snap,
                        &sid,
                    );
                });
            }
            Some(RelayDispatch::Ui(UiEvent::SlotStateChanged {
                slot_id,
                state: st,
                activity,
                conversation_id,
                session_reused,
            }))
        }
        EngineEvent::SlotUsageUpdated { slot_id, usage } => {
            let mut guard = state.lock().await;
            if guard.run_generation != run_gen {
                return None;
            }
            if let Some(slot) = guard
                .config
                .slots
                .iter_mut()
                .find(|s| s.slot_id == slot_id)
            {
                slot.usage = usage.clone();
            }
            Some(RelayDispatch::Ui(UiEvent::SlotUsageUpdated {
                slot_id,
                usage,
            }))
        }
        EngineEvent::ChatMessageAppended(msg) => {
            let mut guard = state.lock().await;
            if guard.run_generation != run_gen {
                return None;
            }
            guard.messages.push(msg.clone());
            Some(RelayDispatch::Ui(UiEvent::ChatMessageAppended(msg)))
        }
        EngineEvent::TasksUpdated(tasks) => {
            let mut guard = state.lock().await;
            if guard.run_generation != run_gen {
                return None;
            }
            guard.tasks = tasks.clone();
            Some(RelayDispatch::Ui(UiEvent::TasksUpdated { tasks }))
        }
        EngineEvent::BlockerNeedsUser(alert) => {
            let mut guard = state.lock().await;
            if guard.run_generation != run_gen {
                return None;
            }
            guard.active_blocker = Some(alert.clone());
            Some(RelayDispatch::Blocker {
                hwnd: guard.main_hwnd,
                alert,
            })
        }
        EngineEvent::Completed(summary) => {
            let mut guard = state.lock().await;
            if guard.run_generation != run_gen {
                return None;
            }
            guard.completion = Some(summary.clone());
            guard.active_blocker = None;
            guard.active_engine_tx = None;
            guard.live_log_sink = None;
            Some(RelayDispatch::Completed {
                hwnd: guard.main_hwnd,
                summary,
            })
        }
        EngineEvent::Toast { level, message } => {
            let guard = state.lock().await;
            if guard.run_generation != run_gen {
                return None;
            }
            Some(RelayDispatch::Toast { level, message })
        }
    }
}

/// Spawns the async task bridging `OrchestratorEngine` events into `AppState`,
/// persistent workspace session storage, native Windows notifications, and WebView IPC.
pub fn spawn_engine_event_relay(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    run_gen: u64,
    mut evt_rx: mpsc::UnboundedReceiver<EngineEvent>,
) {
    tokio::spawn(async move {
        let mut persisted_slots: HashSet<String> = HashSet::new();
        while let Some(evt) = evt_rx.recv().await {
            let Some(action) =
                apply_engine_event_for_generation(&state, run_gen, evt, &mut persisted_slots).await
            else {
                break;
            };
            match action {
                RelayDispatch::Ui(ui_evt) => emit_ui(&proxy, ui_evt),
                RelayDispatch::Blocker { hwnd, alert } => {
                    notify_blocker_needs_user(
                        hwnd,
                        alert.title.clone(),
                        alert.plain_explanation.clone(),
                    );
                    emit_ui(&proxy, UiEvent::BlockerNeedsUser(alert));
                }
                RelayDispatch::Completed { hwnd, summary } => {
                    let popup_text = format_popup_summary(&summary);
                    notify_project_completed(hwnd, popup_text);
                    emit_ui(&proxy, UiEvent::ProjectCompleted(summary));
                }
                RelayDispatch::Toast { level, message } => {
                    toast(&proxy, &level, message);
                }
            }
        }
    });
}
