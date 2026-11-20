use crate::agy::runner::{AgentActivityUpdate, AgyRunner};
use crate::billing::SlotUsageStats;
use crate::orchestrator::engine_roster::ActiveSlotTurn;
use crate::orchestrator::engine_turns::{
    make_system_message, make_user_message, phase_label_zh, FinishedTurn,
};
use crate::orchestrator::engine_usage::SlotBillingLedger;
use crate::orchestrator::schemas::RoleSchemaFiles;
use crate::orchestrator::types::{
    AgentSlot, CompletionSummary, GroupChatMessage, MessageBadge, PermissionMode, RunPhase,
    SameRoleDispatchMode, SlotRunState, TaskItem, UserBlockerAlert,
};
use crate::orchestrator::workspace_guard::workspace_has_shared_build_lock;
use crate::storage::store::{AppStorage, RoomSnapshot};
use chrono::Local;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, Duration, MissedTickBehavior};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub room_id: String,
    pub room_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub goal: String,
    pub permission_mode: PermissionMode,
    pub enable_sandbox: bool,
    pub max_rework_rounds: u8,
    pub turn_timeout_secs: u64,
    pub dispatch_mode: SameRoleDispatchMode,
    pub slots: Vec<AgentSlot>,
    pub schemas: RoleSchemaFiles,
}

#[derive(Debug, Clone)]
pub enum EngineControl {
    Pause,
    Resume,
    Stop,
    PostUserMessage(String),
    UpdateDispatchMode(SameRoleDispatchMode),
    UpdateSlots(Vec<AgentSlot>),
    SetSlotCliAttached { slot_id: String },
    ReleaseSlotCliAttached { slot_id: String },
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    PhaseChanged(RunPhase),
    SlotStateChanged {
        slot_id: String,
        state: SlotRunState,
        activity: Option<String>,
        conversation_id: Option<String>,
        session_reused: bool,
    },
    SlotUsageUpdated {
        slot_id: String,
        usage: SlotUsageStats,
    },
    ChatMessageAppended(GroupChatMessage),
    TasksUpdated(Vec<TaskItem>),
    BlockerNeedsUser(UserBlockerAlert),
    Completed(CompletionSummary),
    Toast { level: String, message: String },
}

pub struct OrchestratorEngine {
    pub(super) cfg: EngineConfig,
    pub(super) runner: Arc<dyn AgyRunner>,
    pub(super) event_tx: mpsc::UnboundedSender<EngineEvent>,
    control_rx: mpsc::UnboundedReceiver<EngineControl>,
    pub(super) activity_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
    activity_rx: mpsc::UnboundedReceiver<AgentActivityUpdate>,
    pub(super) turn_done_tx: mpsc::UnboundedSender<FinishedTurn>,
    turn_done_rx: mpsc::UnboundedReceiver<FinishedTurn>,
    pub(super) cancel_tx: watch::Sender<bool>,
    pub(super) slot_cancel_txs: HashMap<String, ActiveSlotTurn>,
    snapshot_tx: Option<watch::Sender<Option<RoomSnapshot>>>,
    pub(super) usage_ledgers: HashMap<String, SlotBillingLedger>,
    pub(super) turn_seq: u64,
    pub(super) active_architect_turn: Option<u64>,
    pub(super) active_reviewer_turn: Option<u64>,
    pub(super) phase: RunPhase,
    pub(super) slots: Vec<AgentSlot>,
    pub(super) tasks: Vec<TaskItem>,
    pub(super) chat_history: Vec<GroupChatMessage>,
    pub(super) architecture_notes: String,
    pub(super) verification_commands: Vec<String>,
    pub(super) verified_commands: Vec<String>,
    pub(super) pending_user_notes: Vec<String>,
    pub(super) turn_counts: HashMap<String, u32>,
    pub(super) in_flight_turns: usize,
    pub(super) architect_in_flight: bool,
    pub(super) reviewer_in_flight: bool,
    pub(super) initial_plan_requested: bool,
    pub(super) architect_parse_failures: u32,
    pub(super) user_blocker_notified: bool,
    pub(super) started_at: Instant,
    pub(super) shared_build_lock: bool,
    pub(super) board_poll_offset: u64,
    pub(super) seen_chat_ids: HashSet<String>,
}

impl OrchestratorEngine {
    pub fn new(
        cfg: EngineConfig,
        runner: Arc<dyn AgyRunner>,
        event_tx: mpsc::UnboundedSender<EngineEvent>,
        control_rx: mpsc::UnboundedReceiver<EngineControl>,
    ) -> Self {
        let (activity_tx, activity_rx) = mpsc::unbounded_channel();
        let (turn_done_tx, turn_done_rx) = mpsc::unbounded_channel();
        let (cancel_tx, _) = watch::channel(false);
        let (snapshot_tx, _) = watch::channel(None);
        let shared_build_lock = workspace_has_shared_build_lock(&cfg.workspace_dir);
        let board_poll_offset = std::fs::metadata(cfg.room_dir.join("board.jsonl"))
            .map(|m| m.len())
            .unwrap_or(0);
        let mut slots = cfg.slots.clone();
        for slot in &mut slots {
            slot.usage = SlotUsageStats::for_model(&slot.model_id);
        }
        Self {
            cfg,
            runner,
            event_tx,
            control_rx,
            activity_tx,
            activity_rx,
            turn_done_tx,
            turn_done_rx,
            cancel_tx,
            slot_cancel_txs: HashMap::new(),
            snapshot_tx: Some(snapshot_tx),
            usage_ledgers: HashMap::new(),
            turn_seq: 0,
            active_architect_turn: None,
            active_reviewer_turn: None,
            phase: RunPhase::Planning,
            slots,
            tasks: Vec::new(),
            chat_history: Vec::new(),
            architecture_notes: String::new(),
            verification_commands: Vec::new(),
            verified_commands: Vec::new(),
            pending_user_notes: Vec::new(),
            turn_counts: HashMap::new(),
            in_flight_turns: 0,
            architect_in_flight: false,
            reviewer_in_flight: false,
            initial_plan_requested: false,
            architect_parse_failures: 0,
            user_blocker_notified: false,
            started_at: Instant::now(),
            shared_build_lock,
            board_poll_offset,
            seen_chat_ids: HashSet::new(),
        }
    }

    pub async fn run(mut self) {
        let writer_handle = self.snapshot_tx.as_ref().map(|tx| {
            let mut rx = tx.subscribe();
            let room_dir = self.cfg.room_dir.clone();
            tokio::spawn(async move {
                while rx.changed().await.is_ok() {
                    let latest = rx.borrow_and_update().clone();
                    if let Some(snap) = latest {
                        let dir = room_dir.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let _ = AppStorage::save_room_snapshot(&dir, &snap);
                        })
                        .await;
                    }
                }
            })
        });

        self.set_phase(RunPhase::Planning);
        self.emit_chat(make_system_message(
            format!(
                "已启动长期目标自动协同：{}（目录：{}）",
                self.cfg.goal,
                self.cfg.workspace_dir.display()
            ),
            MessageBadge::Info,
            None,
        ));
        self.dispatch_next_work();

        let mut board_poll = interval(Duration::from_millis(1200));
        board_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            if matches!(self.phase, RunPhase::Completed | RunPhase::Stopped)
                && self.in_flight_turns == 0
            {
                break;
            }

            tokio::select! {
                Some(ctrl) = self.control_rx.recv() => {
                    self.handle_control(ctrl);
                    if self.phase == RunPhase::Stopped && self.in_flight_turns == 0 {
                        break;
                    }
                    self.dispatch_next_work();
                }
                Some(act) = self.activity_rx.recv() => {
                    self.handle_activity(act);
                }
                Some(done) = self.turn_done_rx.recv() => {
                    self.handle_finished_turn(done);
                    if matches!(self.phase, RunPhase::Completed | RunPhase::Stopped)
                        && self.in_flight_turns == 0
                    {
                        break;
                    }
                    self.dispatch_next_work();
                }
                _ = board_poll.tick() => {
                    self.poll_external_board_messages();
                }
                else => break,
            }
        }

        // 先优雅收束后台合并写协程，再显式执行一次终态异步刷盘并等待完成，确保零竞态且终态 100% 落盘。
        drop(self.snapshot_tx.take());
        if let Some(handle) = writer_handle {
            let _ = handle.await;
        }
        let final_snap = self.build_room_snapshot();
        let final_dir = self.cfg.room_dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = AppStorage::save_room_snapshot(&final_dir, &final_snap);
        })
        .await;
    }

    fn poll_external_board_messages(&mut self) {
        let path = self.cfg.room_dir.join("board.jsonl");
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        if meta.len() <= self.board_poll_offset {
            return;
        }
        let Ok(mut file) = std::fs::File::open(&path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.board_poll_offset)).is_err() {
            return;
        }
        let mut reader = BufReader::new(file);
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) => break,
                Ok(n) => {
                    if !raw.ends_with(b"\n") {
                        break;
                    }
                    self.board_poll_offset = self.board_poll_offset.saturating_add(n as u64);
                    let decoded = String::from_utf8_lossy(&raw);
                    let trimmed = decoded.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<GroupChatMessage>(trimmed) else {
                        continue;
                    };
                    if !self.seen_chat_ids.insert(msg.id.clone()) {
                        continue;
                    }
                    let is_external_cli = msg
                        .sender_slot_id
                        .as_deref()
                        .map(|s| s.starts_with("cli-"))
                        .unwrap_or(false);
                    if is_external_cli {
                        self.chat_history.push(msg.clone());
                        let _ = self.event_tx.send(EngineEvent::ChatMessageAppended(msg));
                    }
                }
                Err(_) => break,
            }
        }
    }

    pub(super) fn set_phase(&mut self, next: RunPhase) {
        self.phase = next;
        if next != RunPhase::WaitingUser {
            self.user_blocker_notified = false;
        }
        let _ = self.event_tx.send(EngineEvent::PhaseChanged(next));
        self.persist_snapshot();
    }

    pub(super) fn emit_blocker_alert(&mut self, alert: UserBlockerAlert) {
        if self.user_blocker_notified {
            return;
        }
        self.user_blocker_notified = true;
        let _ = self.event_tx.send(EngineEvent::BlockerNeedsUser(alert));
    }

    pub(super) fn emit_chat(&mut self, msg: GroupChatMessage) {
        self.seen_chat_ids.insert(msg.id.clone());
        let _ = AppStorage::append_board_message(&self.cfg.room_dir, &msg);
        self.chat_history.push(msg.clone());
        let _ = self.event_tx.send(EngineEvent::ChatMessageAppended(msg));
    }

    pub(super) fn emit_tasks(&self) {
        let _ = self.event_tx.send(EngineEvent::TasksUpdated(self.tasks.clone()));
        self.persist_snapshot();
    }

    pub(super) fn emit_slot_state(&self, slot: &AgentSlot) {
        let _ = self.event_tx.send(EngineEvent::SlotStateChanged {
            slot_id: slot.slot_id.clone(),
            state: slot.state,
            activity: slot.current_activity.clone(),
            conversation_id: slot.conversation_id.clone(),
            session_reused: slot.session_reused,
        });
    }

    fn build_room_snapshot(&self) -> RoomSnapshot {
        let (total_tokens, total_cost_usd, slot_usages) = self.team_usage_totals();
        RoomSnapshot {
            room_id: self.cfg.room_id.clone(),
            workspace: self.cfg.workspace_dir.to_string_lossy().to_string(),
            goal: self.cfg.goal.clone(),
            phase: phase_label_zh(self.phase).to_string(),
            tasks: self.tasks.clone(),
            updated_at: Local::now().to_rfc3339(),
            total_tokens,
            total_cost_usd,
            slot_usages,
        }
    }

    pub(super) fn persist_snapshot(&self) {
        if let Some(ref tx) = self.snapshot_tx {
            let _ = tx.send(Some(self.build_room_snapshot()));
        }
    }

    fn handle_activity(&mut self, act: AgentActivityUpdate) {
        if let Some(usage) = act.in_flight_usage {
            self.update_slot_in_flight_usage(&act.slot_id, usage, act.peak_context_tokens);
        }
        if let Some(slot) = self.slots.iter_mut().find(|s| s.slot_id == act.slot_id) {
            if let Some(cid) = act.conversation_id {
                slot.conversation_id = Some(cid);
            }
            let text = if act.tool_steps > 0 {
                format!("{}（已执行 {} 步工具操作）", act.summary, act.tool_steps)
            } else {
                act.summary
            };
            slot.current_activity = Some(text);
            let snapshot = slot.clone();
            self.emit_slot_state(&snapshot);
        }
    }

    fn handle_control(&mut self, ctrl: EngineControl) {
        match ctrl {
            EngineControl::Pause => {
                if !matches!(self.phase, RunPhase::Completed | RunPhase::Stopped) {
                    self.set_phase(RunPhase::Paused);
                    self.emit_chat(make_system_message(
                        "已暂停自动派单（当前进行中的轮次将优雅收敛后停靠）。",
                        MessageBadge::Info,
                        None,
                    ));
                }
            }
            EngineControl::Resume => {
                if matches!(self.phase, RunPhase::Paused | RunPhase::WaitingUser) {
                    let from_waiting = self.phase == RunPhase::WaitingUser;
                    let mut resumed_slots = Vec::new();
                    for slot in &mut self.slots {
                        if slot.state == SlotRunState::Paused
                            || (from_waiting
                                && matches!(
                                    slot.state,
                                    SlotRunState::QuotaCooling | SlotRunState::Error
                                ))
                        {
                            slot.state = SlotRunState::Idle;
                            slot.consecutive_failures = 0;
                            resumed_slots.push(slot.clone());
                        }
                    }
                    for snap in resumed_slots {
                        self.emit_slot_state(&snap);
                    }
                    let target_phase = if self.tasks.is_empty() {
                        RunPhase::Planning
                    } else {
                        RunPhase::Running
                    };
                    self.set_phase(target_phase);
                    self.emit_chat(make_system_message(
                        "已恢复群组自动协同。",
                        MessageBadge::Info,
                        None,
                    ));
                }
            }
            EngineControl::Stop => {
                let _ = self.cancel_tx.send(true);
                for active in self.slot_cancel_txs.drain().map(|(_, v)| v) {
                    let _ = active.cancel_tx.send(true);
                }
                self.settle_all_in_flight_usages();
                self.set_phase(RunPhase::Stopped);
                self.emit_chat(make_system_message(
                    "已停止本次协同任务，并终止所有后台模型子进程。",
                    MessageBadge::Warning,
                    None,
                ));
            }
            EngineControl::PostUserMessage(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return;
                }
                self.emit_chat(make_user_message(trimmed));
                self.pending_user_notes.push(trimmed.to_string());
                if self.architect_in_flight {
                    self.emit_chat(make_system_message(
                        "已收到您的指导意见，总架构师当前轮次收敛后将第一时间优先响应（如需立即打断重排可点暂停/恢复）。",
                        MessageBadge::Info,
                        None,
                    ));
                }
                if self.phase == RunPhase::WaitingUser {
                    let target_phase = if self.tasks.is_empty() {
                        RunPhase::Planning
                    } else {
                        RunPhase::Running
                    };
                    self.set_phase(target_phase);
                }
            }
            EngineControl::UpdateDispatchMode(mode) => {
                self.cfg.dispatch_mode = mode;
            }
            EngineControl::UpdateSlots(updated) => {
                self.apply_updated_slots(updated);
            }
            EngineControl::SetSlotCliAttached { slot_id } => {
                self.attach_slot_external_cli(&slot_id);
            }
            EngineControl::ReleaseSlotCliAttached { slot_id } => {
                self.release_slot_external_cli(&slot_id);
            }
        }
    }
}
