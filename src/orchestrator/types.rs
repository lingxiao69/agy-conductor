use crate::billing::SlotUsageStats;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoleKind {
    /// 总架构规划师：全局勘察、制定架构契约、拆解子任务、争议仲裁与终验
    Architect,
    /// 编码程序员：实现子任务、运行局部自测、响应审查打回返工
    Coder,
    /// 项目代码审查员：只读审查 git diff、执行验收测试、挑刺打回或放行
    Reviewer,
}

impl RoleKind {
    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Architect => "总架构师",
            Self::Coder => "编码程序员",
            Self::Reviewer => "代码审查员",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotRunState {
    Idle,
    Working,
    QuotaCooling,
    Paused,
    ExternalCliAttached,
    Error,
}

impl SlotRunState {
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::QuotaCooling | Self::Paused | Self::ExternalCliAttached | Self::Error
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSlot {
    pub slot_id: String,
    pub alias: String,
    pub display_name: String,
    pub model_id: String,
    pub role: RoleKind,
    /// 1..=99, smaller number means higher priority.
    pub priority: u8,
    pub enabled: bool,
    #[serde(default = "default_slot_state")]
    pub state: SlotRunState,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub session_reused: bool,
    #[serde(default)]
    pub current_activity: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub usage: SlotUsageStats,
}

fn default_slot_state() -> SlotRunState {
    SlotRunState::Idle
}

impl AgentSlot {
    pub fn normalized_priority(&self) -> u8 {
        self.priority.clamp(1, 99)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SameRoleDispatchMode {
    /// 优先级优先 + 空闲协同分担（并行溢出）
    PriorityWithOverflow,
    /// 严格主备容灾（仅主力不可用时切换备机）
    StrictPrimaryFailover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    /// 文件编辑自动放行 (`--mode accept-edits`)
    AcceptEdits,
    /// 无人值守全自动测试与构建 (`--dangerously-skip-permissions`)
    DangerouslySkipPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    WaitingPathLock,
    Coding,
    PendingReview,
    InReview,
    ReworkNeeded,
    Approved,
    BlockedByArch,
    NeedsUserDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewIssue {
    pub severity: String,
    pub file_path: String,
    #[serde(default)]
    pub line_hint: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskItem {
    pub task_id: String,
    pub title: String,
    pub description: String,
    pub owned_paths: Vec<String>,
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub preferred_coder_alias: Option<String>,
    pub acceptance_criteria: Vec<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub assigned_coder_slot_id: Option<String>,
    #[serde(default)]
    pub assigned_coder_label: Option<String>,
    #[serde(default)]
    pub last_coder_slot_id: Option<String>,
    #[serde(default)]
    pub rework_count: u8,
    #[serde(default)]
    pub arch_arbitration_count: u8,
    #[serde(default)]
    pub coder_failure_streak: u8,
    #[serde(default)]
    pub last_coder_summary: Option<String>,
    #[serde(default)]
    pub last_modified_files: Vec<String>,
    #[serde(default)]
    pub last_self_check: Option<String>,
    #[serde(default)]
    pub last_review_issues: Vec<ReviewIssue>,
    #[serde(default)]
    pub last_review_note: Option<String>,
    #[serde(default)]
    pub blocker_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunPhase {
    Idle,
    Planning,
    Running,
    FinalGate,
    Paused,
    WaitingUser,
    Completed,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageBadge {
    None,
    Plan,
    Submit,
    Approved,
    Rework,
    Blocked,
    FinalAccept,
    User,
    Info,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupChatMessage {
    pub id: String,
    pub timestamp: String,
    #[serde(default)]
    pub sender_slot_id: Option<String>,
    pub sender_alias: String,
    pub sender_display: String,
    #[serde(default)]
    pub sender_role: Option<RoleKind>,
    #[serde(default)]
    pub priority: Option<u8>,
    #[serde(default)]
    pub is_user: bool,
    #[serde(default)]
    pub is_system: bool,
    pub content: String,
    #[serde(default = "default_badge")]
    pub badge: MessageBadge,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub modified_files: Vec<String>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub tool_steps: Option<u32>,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
}

fn default_badge() -> MessageBadge {
    MessageBadge::None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotContribution {
    pub slot_id: String,
    pub display_name: String,
    pub role: RoleKind,
    pub turns: u32,
    #[serde(default)]
    pub usage: SlotUsageStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionSummary {
    pub workspace: String,
    pub goal: String,
    pub total_duration_seconds: u64,
    pub total_tasks: usize,
    pub total_rework_rounds: u32,
    pub verified_commands: Vec<String>,
    pub contributions: Vec<SlotContribution>,
    pub architect_note: String,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserBlockerAlert {
    pub title: String,
    pub plain_explanation: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub reason_kind: String,
}
