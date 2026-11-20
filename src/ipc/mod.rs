mod cli_hub_actions;
pub mod dispatcher;
mod env_actions;
mod history_preset_actions;
mod run_actions;
pub mod run_events;
pub mod session_terminal_actions;
pub mod state;
mod usage_actions;

use crate::agy::live_log::LiveTerminalLine;
use crate::agy::models::ModelInfo;
use crate::agy::resolver::DoctorReport;
use crate::billing::SlotUsageStats;
use crate::cli_hub::{CliProviderId, CliProviderStatus};
use crate::orchestrator::types::{
    AgentSlot, CompletionSummary, GroupChatMessage, PermissionMode, RunPhase,
    SameRoleDispatchMode, SlotRunState, TaskItem, UserBlockerAlert,
};
use crate::storage::history::{HistoryRoomDetail, HistoryRoomSummary};
use crate::storage::presets::RosterPreset;
use crate::storage::workspace_sessions::WorkspaceRoleSession;
use crate::usage_stats::{CliUsageOverview, UsageRange};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

fn default_reuse_true() -> bool {
    true
}

/// Commands sent from the WebView UI (`window.ipc.postMessage`) to Rust.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum UiCommand {
    InitApp,
    RefreshModels,
    PickWorkspaceFolder,
    SetWorkspaceFolder {
        workspace: String,
    },
    DiscoverWorkspaceSessions {
        workspace: String,
    },
    SetReuseRoleSessions {
        reuse: bool,
    },
    ClearTerminalLog,
    TogglePreferGateway {
        prefer_gateway: bool,
    },
    ApplyDefaultRoster,
    SaveRoster {
        slots: Vec<AgentSlot>,
        dispatch_mode: SameRoleDispatchMode,
    },
    SaveRosterPreset {
        name: String,
        slots: Vec<AgentSlot>,
        dispatch_mode: SameRoleDispatchMode,
    },
    ApplyRosterPreset {
        preset_id: String,
    },
    DeleteRosterPreset {
        preset_id: String,
    },
    ListHistoryRooms,
    LoadHistoryRoom {
        room_id: String,
    },
    DeleteHistoryRoom {
        room_id: String,
    },
    ExportHistoryMarkdown {
        room_id: String,
    },
    StartGoal {
        workspace: String,
        goal: String,
        permission_mode: PermissionMode,
        enable_sandbox: bool,
        max_rework_rounds: u8,
        #[serde(default = "default_reuse_true")]
        reuse_role_sessions: bool,
        #[serde(default)]
        selected_sessions: HashMap<String, String>,
    },
    PauseRun,
    ResumeRun,
    StopRun,
    ResetRun,
    PostUserMessage {
        text: String,
    },
    OpenTerminal {
        slot_id: Option<String>,
        attach_conversation: bool,
        split_all: bool,
        mount_mcp: bool,
        #[serde(default)]
        watch_live: bool,
    },
    ReleaseTerminalAttach {
        slot_id: String,
    },
    RunAgyUpdate,
    RunDoctor,
    OpenWorkspaceExplorer,
    SelectCliProvider {
        provider: CliProviderId,
    },
    RefreshCliProviders,
    LaunchProviderExternalCli {
        provider: CliProviderId,
    },
    LoadCliUsageStats {
        #[serde(default)]
        range: UsageRange,
        #[serde(default)]
        force_refresh: bool,
    },
    ExportReportMarkdown {
        title: String,
        markdown: String,
    },
}

/// Events pushed from Rust to the WebView UI (`window.__agyDispatch(...)`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    StateSnapshot(FullUiState),
    CliProvidersUpdated {
        active_provider: CliProviderId,
        providers: Vec<CliProviderStatus>,
    },
    WorkspaceSelected {
        workspace: String,
    },
    WorkspaceSessionsDiscovered {
        workspace: String,
        reuse_role_sessions: bool,
        sessions: Vec<WorkspaceRoleSession>,
    },
    ModelsUpdated {
        version: String,
        runtime_kind: String,
        runtime_path: String,
        models: Vec<ModelInfo>,
        prefer_gateway: bool,
        slots: Vec<AgentSlot>,
    },
    DoctorUpdated(DoctorReport),
    PhaseChanged {
        phase: RunPhase,
    },
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
    TasksUpdated {
        tasks: Vec<TaskItem>,
    },
    TerminalLineAppended(LiveTerminalLine),
    TerminalLogCleared,
    BlockerNeedsUser(UserBlockerAlert),
    HistoryRoomsListed {
        rooms: Vec<HistoryRoomSummary>,
    },
    HistoryRoomLoaded {
        detail: HistoryRoomDetail,
    },
    HistoryRoomDeleted {
        room_id: String,
        rooms: Vec<HistoryRoomSummary>,
    },
    UpdateLogChunk {
        line: String,
        finished: bool,
        new_version: Option<String>,
    },
    ProjectCompleted(CompletionSummary),
    CliUsageStatsLoaded {
        overview: CliUsageOverview,
    },
    SystemToast {
        level: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct FullUiState {
    pub agy_version: String,
    pub runtime_kind: String,
    pub runtime_path: String,
    pub active_cli_provider: CliProviderId,
    pub cli_providers: Vec<CliProviderStatus>,
    pub terminal_host: String,
    pub supports_split_pane: bool,
    pub prefer_gateway: bool,
    pub workspace: String,
    pub goal: String,
    pub phase: RunPhase,
    pub dispatch_mode: SameRoleDispatchMode,
    pub permission_mode: PermissionMode,
    pub enable_sandbox: bool,
    pub max_rework_rounds: u8,
    pub reuse_role_sessions: bool,
    pub workspace_sessions: Vec<WorkspaceRoleSession>,
    pub terminal_lines: Vec<LiveTerminalLine>,
    pub active_blocker: Option<UserBlockerAlert>,
    pub models: Vec<ModelInfo>,
    pub slots: Vec<AgentSlot>,
    pub presets: Vec<RosterPreset>,
    pub active_preset_id: Option<String>,
    pub tasks: Vec<TaskItem>,
    pub messages: Vec<GroupChatMessage>,
    pub completion: Option<CompletionSummary>,
    pub doctor: Option<DoctorReport>,
}

/// Events sent to the `tao` main event loop from background Tokio tasks.
#[derive(Debug)]
pub enum AppCustomEvent {
    DispatchUiEvent(UiEvent),
    UiCommandReceived(String),
}
