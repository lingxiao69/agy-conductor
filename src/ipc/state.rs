use crate::agy::gateway_wildcards::sanitize_wildcard_model_id;
use crate::agy::live_log::{LiveLogSink, LiveTerminalLine};
use crate::agy::models::{build_default_roster, ModelInfo};
use crate::agy::resolver::{detect_user_prefers_gateway, DoctorReport, ResolvedAgyRuntime};
use crate::agy::terminal::detect_terminal_host;
use crate::billing::SlotUsageStats;
use crate::cli_hub::{default_cli_providers, CliProviderStatus};
use crate::ipc::FullUiState;
use crate::orchestrator::engine::EngineControl;
use crate::orchestrator::types::{
    AgentSlot, CompletionSummary, GroupChatMessage, PermissionMode, RunPhase, SameRoleDispatchMode,
    SlotRunState, TaskItem, UserBlockerAlert,
};
use crate::storage::presets::{
    delete_preset, find_preset, sanitize_slots_for_preset, save_preset, RosterPreset,
};
use crate::storage::store::{AppStorage, PersistedConfig};
use crate::storage::workspace_sessions::WorkspaceRoleSession;
use crate::win_guard::job_object::SharedJobGuard;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

pub const MAX_TERMINAL_RING_LINES: usize = 600;

pub struct AppCoreState {
    pub storage: AppStorage,
    pub config: PersistedConfig,
    pub runtime: Option<ResolvedAgyRuntime>,
    pub agy_version: String,
    pub runtime_kind: String,
    pub runtime_path: String,
    pub cli_providers: Vec<CliProviderStatus>,
    pub models: Vec<ModelInfo>,
    pub doctor: Option<DoctorReport>,
    pub current_room_id: Option<String>,
    pub current_room_dir: Option<PathBuf>,
    pub goal: String,
    pub phase: RunPhase,
    pub tasks: Vec<TaskItem>,
    pub messages: Vec<GroupChatMessage>,
    pub workspace_sessions: Vec<WorkspaceRoleSession>,
    pub terminal_lines: VecDeque<LiveTerminalLine>,
    pub active_blocker: Option<UserBlockerAlert>,
    pub live_log_sink: Option<Arc<LiveLogSink>>,
    pub completion: Option<CompletionSummary>,
    pub active_engine_tx: Option<mpsc::UnboundedSender<EngineControl>>,
    pub job_guard: Option<SharedJobGuard>,
    pub main_hwnd: Option<isize>,
    pub update_in_progress: bool,
}

impl AppCoreState {
    pub fn new(
        storage: AppStorage,
        job_guard: Option<SharedJobGuard>,
        main_hwnd: Option<isize>,
    ) -> Self {
        let mut config = storage.load_config();
        let mut config_repaired = false;
        if config.slots.is_empty() {
            config.prefer_gateway_models = detect_user_prefers_gateway();
            config.slots = build_default_roster(&[], config.prefer_gateway_models);
            config.active_preset_id = None;
        } else {
            for slot in &mut config.slots {
                let cleaned = sanitize_wildcard_model_id(&slot.model_id);
                if cleaned != slot.model_id {
                    slot.model_id = cleaned;
                    config_repaired = true;
                }
                slot.state = SlotRunState::Idle;
                slot.session_reused = false;
                slot.current_activity = None;
                slot.consecutive_failures = 0;
                slot.usage = SlotUsageStats::for_model(&slot.model_id);
            }
        }
        for preset in &mut config.presets {
            for slot in &mut preset.slots {
                let cleaned = sanitize_wildcard_model_id(&slot.model_id);
                if cleaned != slot.model_id {
                    slot.model_id = cleaned;
                    slot.usage = SlotUsageStats::for_model(&slot.model_id);
                    config_repaired = true;
                }
            }
        }
        if let Some(ref active_id) = config.active_preset_id {
            if !config.presets.iter().any(|p| &p.preset_id == active_id) {
                config.active_preset_id = None;
            }
        }
        if config_repaired {
            let _ = storage.save_config(&config);
        }
        Self {
            storage,
            config,
            runtime: None,
            agy_version: "检测中...".to_string(),
            runtime_kind: "检测中...".to_string(),
            runtime_path: String::new(),
            cli_providers: default_cli_providers(),
            models: Vec::new(),
            doctor: None,
            current_room_id: None,
            current_room_dir: None,
            goal: String::new(),
            phase: RunPhase::Idle,
            tasks: Vec::new(),
            messages: Vec::new(),
            workspace_sessions: Vec::new(),
            terminal_lines: VecDeque::with_capacity(MAX_TERMINAL_RING_LINES),
            active_blocker: None,
            live_log_sink: None,
            completion: None,
            active_engine_tx: None,
            job_guard,
            main_hwnd,
            update_in_progress: false,
        }
    }

    pub fn snapshot(&self) -> FullUiState {
        let host = detect_terminal_host();
        FullUiState {
            agy_version: self.agy_version.clone(),
            runtime_kind: self.runtime_kind.clone(),
            runtime_path: self.runtime_path.clone(),
            active_cli_provider: self.config.active_cli_provider,
            cli_providers: self.cli_providers.clone(),
            terminal_host: host.display_name(),
            supports_split_pane: host.supports_split_pane(),
            prefer_gateway: self.config.prefer_gateway_models,
            workspace: self.config.last_workspace.clone(),
            goal: self.goal.clone(),
            phase: self.phase,
            dispatch_mode: self.config.dispatch_mode,
            permission_mode: self.config.permission_mode,
            enable_sandbox: self.config.enable_sandbox,
            max_rework_rounds: self.config.max_rework_rounds,
            reuse_role_sessions: self.config.reuse_role_sessions,
            workspace_sessions: self.workspace_sessions.clone(),
            terminal_lines: self.terminal_lines.iter().cloned().collect(),
            active_blocker: self.active_blocker.clone(),
            models: self.models.clone(),
            slots: self.config.slots.clone(),
            presets: self.config.presets.clone(),
            active_preset_id: self.config.active_preset_id.clone(),
            tasks: self.tasks.clone(),
            messages: self.messages.clone(),
            completion: self.completion.clone(),
            doctor: self.doctor.clone(),
        }
    }

    pub fn push_terminal_line(&mut self, line: LiveTerminalLine) {
        self.terminal_lines.push_back(line);
        while self.terminal_lines.len() > MAX_TERMINAL_RING_LINES {
            self.terminal_lines.pop_front();
        }
    }

    pub fn sync_live_log_slots(&self) {
        if let Some(ref sink) = self.live_log_sink {
            sink.sync_slots(&self.config.slots);
        }
    }

    pub fn save_persisted_config(&self) {
        let _ = self.storage.save_config(&self.config);
    }

    pub fn is_run_active(&self) -> bool {
        self.active_engine_tx.is_some()
            && matches!(
                self.phase,
                RunPhase::Planning
                    | RunPhase::Running
                    | RunPhase::FinalGate
                    | RunPhase::Paused
                    | RunPhase::WaitingUser
            )
    }

    pub fn is_room_currently_running(&self, room_id: &str) -> bool {
        self.is_run_active() && self.current_room_id.as_deref() == Some(room_id)
    }

    pub fn set_slots_and_mode(&mut self, slots: Vec<AgentSlot>, mode: SameRoleDispatchMode) {
        let existing: HashMap<&str, &AgentSlot> = self
            .config
            .slots
            .iter()
            .map(|s| (s.slot_id.as_str(), s))
            .collect();

        let normalized: Vec<AgentSlot> = slots
            .into_iter()
            .map(|mut s| {
                s.model_id = sanitize_wildcard_model_id(&s.model_id);
                s.priority = s.normalized_priority();
                if let Some(prev) = existing.get(s.slot_id.as_str()) {
                    let identity_changed = prev.model_id != s.model_id || prev.role != s.role;
                    if identity_changed {
                        s.state = SlotRunState::Idle;
                        s.conversation_id = None;
                        s.session_reused = false;
                        s.current_activity = None;
                        s.usage = SlotUsageStats::for_model(&s.model_id);
                    } else {
                        s.state = prev.state;
                        if s.conversation_id.is_none() {
                            s.conversation_id = prev.conversation_id.clone();
                        }
                        s.session_reused = prev.session_reused;
                        s.current_activity = prev.current_activity.clone();
                        s.usage = prev.usage.clone();
                    }
                } else {
                    s.session_reused = false;
                    s.usage = SlotUsageStats::for_model(&s.model_id);
                }
                s
            })
            .collect();

        self.config.slots = normalized;
        self.config.dispatch_mode = mode;
        self.config.active_preset_id = None;
        self.save_persisted_config();
        self.sync_live_log_slots();
    }

    pub fn apply_default_roster(&mut self) {
        let prefer = self.config.prefer_gateway_models;
        self.config.slots = build_default_roster(&self.models, prefer);
        self.config.active_preset_id = None;
        self.save_persisted_config();
    }

    pub fn save_roster_preset(
        &mut self,
        name: &str,
        slots: Vec<AgentSlot>,
        dispatch_mode: SameRoleDispatchMode,
    ) -> Result<(RosterPreset, bool), String> {
        let result = save_preset(&mut self.config, name, slots, dispatch_mode)?;
        self.storage.save_config(&self.config)?;
        Ok(result)
    }

    /// 切换并应用命名编队预设：
    /// - 非运行态：清空所有槽位旧 `conversation_id` 与运行态；
    /// - 运行态：仅在同 `slot_id` 且 `model_id`、`role` 均未变时保留运行态，并同步通知 `EngineControl`。
    pub fn apply_roster_preset(&mut self, preset_id: &str) -> Result<RosterPreset, String> {
        let preset = find_preset(&self.config, preset_id)
            .cloned()
            .ok_or_else(|| format!("未找到指定的编队预设: {preset_id}"))?;

        let mut next_slots = sanitize_slots_for_preset(preset.slots.clone());
        if self.is_run_active() {
            let existing: HashMap<&str, &AgentSlot> = self
                .config
                .slots
                .iter()
                .map(|s| (s.slot_id.as_str(), s))
                .collect();
            for slot in &mut next_slots {
                if let Some(prev) = existing.get(slot.slot_id.as_str()) {
                    if prev.model_id == slot.model_id && prev.role == slot.role {
                        slot.state = prev.state;
                        slot.conversation_id = prev.conversation_id.clone();
                        slot.session_reused = prev.session_reused;
                        slot.current_activity = prev.current_activity.clone();
                        slot.consecutive_failures = prev.consecutive_failures;
                        slot.usage = prev.usage.clone();
                    }
                }
            }
        }

        self.config.slots = next_slots;
        self.config.dispatch_mode = preset.dispatch_mode;
        self.config.active_preset_id = Some(preset.preset_id.clone());
        self.storage.save_config(&self.config)?;
        self.sync_live_log_slots();
        self.sync_roster_to_active_engine();
        Ok(preset)
    }

    pub fn sync_roster_to_active_engine(&self) {
        if self.is_run_active() {
            if let Some(ref tx) = self.active_engine_tx {
                let _ = tx.send(EngineControl::UpdateDispatchMode(self.config.dispatch_mode));
                let _ = tx.send(EngineControl::UpdateSlots(self.config.slots.clone()));
            }
        }
    }

    pub fn delete_roster_preset(&mut self, preset_id: &str) -> Result<RosterPreset, String> {
        let removed = delete_preset(&mut self.config, preset_id)?;
        self.storage.save_config(&self.config)?;
        Ok(removed)
    }

    pub fn apply_run_preferences(
        &mut self,
        workspace: String,
        goal: String,
        permission_mode: PermissionMode,
        enable_sandbox: bool,
        max_rework_rounds: u8,
        reuse_role_sessions: bool,
    ) {
        self.config.last_workspace = workspace;
        self.config.permission_mode = permission_mode;
        self.config.enable_sandbox = enable_sandbox;
        self.config.max_rework_rounds = max_rework_rounds.clamp(1, 20);
        self.config.reuse_role_sessions = reuse_role_sessions;
        self.goal = goal;
        self.save_persisted_config();
    }
}

pub type SharedAppState = Arc<Mutex<AppCoreState>>;

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
