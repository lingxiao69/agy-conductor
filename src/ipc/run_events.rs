use crate::ipc::dispatcher::{emit_ui, toast};
use crate::ipc::session_terminal_actions::persist_slot_session_update;
use crate::ipc::state::SharedAppState;
use crate::ipc::{AppCustomEvent, UiEvent};
use crate::orchestrator::engine::EngineEvent;
use crate::orchestrator::engine_turns::format_popup_summary;
use crate::orchestrator::types::{RunPhase, SlotRunState};
use crate::win_guard::notifier::{notify_blocker_needs_user, notify_project_completed};
use std::collections::HashSet;
use tao::event_loop::EventLoopProxy;
use tokio::sync::mpsc;

/// Spawns the async task bridging `OrchestratorEngine` events into `AppState`,
/// persistent workspace session storage, native Windows notifications, and WebView IPC.
pub fn spawn_engine_event_relay(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    mut evt_rx: mpsc::UnboundedReceiver<EngineEvent>,
) {
    tokio::spawn(async move {
        let mut persisted_slots: HashSet<String> = HashSet::new();
        while let Some(evt) = evt_rx.recv().await {
            match evt {
                EngineEvent::PhaseChanged(phase) => {
                    {
                        let mut guard = state.lock().await;
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
                    }
                    emit_ui(&proxy, UiEvent::PhaseChanged { phase });
                }
                EngineEvent::SlotStateChanged {
                    slot_id,
                    state: st,
                    activity,
                    conversation_id,
                    session_reused,
                } => {
                    handle_slot_state_changed(
                        &state,
                        &proxy,
                        &mut persisted_slots,
                        slot_id,
                        st,
                        activity,
                        conversation_id,
                        session_reused,
                    )
                    .await;
                }
                EngineEvent::SlotUsageUpdated { slot_id, usage } => {
                    {
                        let mut guard = state.lock().await;
                        if let Some(slot) = guard
                            .config
                            .slots
                            .iter_mut()
                            .find(|s| s.slot_id == slot_id)
                        {
                            slot.usage = usage.clone();
                        }
                    }
                    emit_ui(&proxy, UiEvent::SlotUsageUpdated { slot_id, usage });
                }
                EngineEvent::ChatMessageAppended(msg) => {
                    {
                        state.lock().await.messages.push(msg.clone());
                    }
                    emit_ui(&proxy, UiEvent::ChatMessageAppended(msg));
                }
                EngineEvent::TasksUpdated(tasks) => {
                    {
                        state.lock().await.tasks = tasks.clone();
                    }
                    emit_ui(&proxy, UiEvent::TasksUpdated { tasks });
                }
                EngineEvent::BlockerNeedsUser(alert) => {
                    let hwnd = {
                        let mut guard = state.lock().await;
                        guard.active_blocker = Some(alert.clone());
                        guard.main_hwnd
                    };
                    notify_blocker_needs_user(
                        hwnd,
                        alert.title.clone(),
                        alert.plain_explanation.clone(),
                    );
                    emit_ui(&proxy, UiEvent::BlockerNeedsUser(alert));
                }
                EngineEvent::Completed(summary) => {
                    let hwnd = {
                        let mut guard = state.lock().await;
                        guard.completion = Some(summary.clone());
                        guard.active_blocker = None;
                        guard.active_engine_tx = None;
                        guard.live_log_sink = None;
                        guard.main_hwnd
                    };
                    let popup_text = format_popup_summary(&summary);
                    notify_project_completed(hwnd, popup_text);
                    emit_ui(&proxy, UiEvent::ProjectCompleted(summary));
                }
                EngineEvent::Toast { level, message } => {
                    toast(&proxy, &level, message);
                }
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn handle_slot_state_changed(
    state: &SharedAppState,
    proxy: &EventLoopProxy<AppCustomEvent>,
    persisted_slots: &mut HashSet<String>,
    slot_id: String,
    st: SlotRunState,
    activity: Option<String>,
    conversation_id: Option<String>,
    session_reused: bool,
) {
    let persist_ctx = {
        let mut guard = state.lock().await;
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
    emit_ui(
        proxy,
        UiEvent::SlotStateChanged {
            slot_id,
            state: st,
            activity,
            conversation_id,
            session_reused,
        },
    );
}
