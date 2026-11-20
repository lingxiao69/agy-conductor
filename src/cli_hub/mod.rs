pub mod detector;

pub use detector::{
    default_cli_providers, detect_all_cli_providers, launch_provider_external_cli,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CliProviderId {
    #[default]
    Agy,
    Codex,
    ClaudeCode,
    #[serde(rename = "opencode")]
    OpenCode,
}

impl CliProviderId {
    pub fn all() -> [CliProviderId; 4] {
        [
            CliProviderId::Agy,
            CliProviderId::Codex,
            CliProviderId::ClaudeCode,
            CliProviderId::OpenCode,
        ]
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Agy => "Antigravity (agy)",
            Self::Codex => "Codex CLI",
            Self::ClaudeCode => "Claude Code",
            Self::OpenCode => "OpenCode",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude",
            Self::OpenCode => "OpenCode",
        }
    }

    pub fn command_name(self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
            Self::OpenCode => "opencode",
        }
    }

    pub fn tagline(self) -> &'static str {
        match self {
            Self::Agy => "主力全自动多角色编队引擎 · 规划/编码/审查/实时流与官方单价计费已就绪",
            Self::Codex => "OpenAI 终端编码引擎 · 支持本机探测与独立终端拉起，多角色编排适配器预留中",
            Self::ClaudeCode => "Anthropic Claude 终端引擎 · 支持本机探测与独立终端拉起，多角色编排适配器预留中",
            Self::OpenCode => "开源多模型终端编码助手 · 支持本机探测与独立终端拉起，多角色编排适配器预留中",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliSupportTier {
    ActiveOrchestrator,
    PreviewDetected,
    Planned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliProviderStatus {
    pub id: CliProviderId,
    pub display_name: String,
    pub short_label: String,
    pub command_name: String,
    pub installed: bool,
    pub executable_path: Option<String>,
    pub version_hint: Option<String>,
    pub support_tier: CliSupportTier,
    pub tagline: String,
}
