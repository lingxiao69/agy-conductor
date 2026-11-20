use crate::agy::gateway_wildcards::{
    prepare_gateway_session_settings, sanitize_wildcard_model_id,
};
use crate::agy::live_log::{sanitize_slot_filename_id, LiveLogSink, LiveStreamKind};
use crate::agy::resolver::ResolvedAgyRuntime;
use crate::agy::runner_attempt::{execute_single_attempt, AttemptContext, AttemptOutcome};
use crate::agy::stream_parser::{AgentErrorKind, TokenUsage};
use crate::agy::usage_tracker::DEFAULT_FIRST_PROGRESS_WATCHDOG_SECS;
use crate::orchestrator::types::PermissionMode;
use crate::win_guard::job_object::SharedJobGuard;
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tokio::time::Duration;

#[derive(Debug, Clone)]
pub struct AgentTurnRequest {
    pub slot_id: String,
    pub model_id: String,
    pub workspace_dir: PathBuf,
    pub schema_path: PathBuf,
    pub prompt: String,
    pub conversation_id: Option<String>,
    pub permission_mode: PermissionMode,
    pub enable_sandbox: bool,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub struct AgentActivityUpdate {
    pub slot_id: String,
    pub summary: String,
    pub tool_steps: u32,
    pub conversation_id: Option<String>,
    pub in_flight_usage: Option<TokenUsage>,
    pub peak_context_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct AgentTurnResponse {
    pub conversation_id: Option<String>,
    pub structured_json: Value,
    pub raw_response: String,
    pub duration_seconds: f64,
    pub tool_steps: u32,
    pub usage: TokenUsage,
    pub peak_context_tokens: u64,
    pub switched_to_fresh_session: bool,
}

#[derive(Debug, Clone)]
pub struct AgentTurnError {
    pub kind: AgentErrorKind,
    pub message: String,
    pub conversation_id: Option<String>,
    pub partial_usage: Option<TokenUsage>,
    pub peak_context_tokens: u64,
}

impl std::fmt::Display for AgentTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}", self.kind, self.message)
    }
}

pub type TurnFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AgentTurnResponse, AgentTurnError>> + Send + 'a>>;

pub trait AgyRunner: Send + Sync {
    fn run_turn<'a>(
        &'a self,
        request: AgentTurnRequest,
        activity_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a>;
}

pub struct RealAgyRunner {
    runtime: ResolvedAgyRuntime,
    job_guard: Option<SharedJobGuard>,
    live_log: Option<Arc<LiveLogSink>>,
    first_progress_watchdog: Duration,
    brain_dir_override: Option<PathBuf>,
}

impl RealAgyRunner {
    pub fn new(runtime: ResolvedAgyRuntime, job_guard: Option<SharedJobGuard>) -> Self {
        Self::with_live_log(runtime, job_guard, None)
    }

    pub fn with_live_log(
        runtime: ResolvedAgyRuntime,
        job_guard: Option<SharedJobGuard>,
        live_log: Option<Arc<LiveLogSink>>,
    ) -> Self {
        Self {
            runtime,
            job_guard,
            live_log,
            first_progress_watchdog: Duration::from_secs(DEFAULT_FIRST_PROGRESS_WATCHDOG_SECS),
            brain_dir_override: None,
        }
    }

    pub fn with_first_progress_watchdog(mut self, dur: Duration) -> Self {
        self.first_progress_watchdog = dur;
        self
    }

    pub fn with_brain_dir_override(mut self, dir: PathBuf) -> Self {
        self.brain_dir_override = Some(dir);
        self
    }

    /// 构造无头流式执行参数（严禁传入 `--print` 或 `-p`：`agy v1.5.13` 中 `--print` 为带参字符串选项会吞掉后续参数，
    /// 仅传 `--input-format stream-json --output-format stream-json` 即可进入无头流式模式）。
    pub fn build_turn_args(request: &AgentTurnRequest) -> Vec<String> {
        let safe_model_id = sanitize_wildcard_model_id(&request.model_id);
        let mut args = vec![
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--model".to_string(),
            safe_model_id.clone(),
            "--json-schema".to_string(),
            request.schema_path.to_string_lossy().to_string(),
            "--disable-slash-commands".to_string(),
        ];

        if let Some(output_dir) = request.schema_path.parent().filter(|p| p.is_dir()) {
            let stem = sanitize_slot_filename_id(&request.slot_id);
            if let Ok(Some(settings_path)) =
                prepare_gateway_session_settings(&safe_model_id, output_dir, &stem, None, None)
            {
                args.push("--settings".to_string());
                args.push(settings_path.to_string_lossy().to_string());
            }
        }

        if let Some(conv_id) = request
            .conversation_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            args.push("--conversation".to_string());
            args.push(conv_id.to_string());
        }

        match request.permission_mode {
            PermissionMode::AcceptEdits => {
                args.push("--mode".to_string());
                args.push("accept-edits".to_string());
            }
            PermissionMode::DangerouslySkipPermissions => {
                args.push("--dangerously-skip-permissions".to_string());
            }
        }

        if request.enable_sandbox {
            args.push("--sandbox".to_string());
        }

        args
    }

    fn attempt_context(&self) -> AttemptContext<'_> {
        AttemptContext {
            runtime: &self.runtime,
            job_guard: self.job_guard.as_ref(),
            live_log: self.live_log.as_ref(),
            first_progress_watchdog: self.first_progress_watchdog,
            brain_dir_override: self.brain_dir_override.clone(),
        }
    }

    fn log_live(&self, slot_id: &str, kind: LiveStreamKind, text: impl Into<String>) {
        if let Some(sink) = &self.live_log {
            sink.append_for_slot(slot_id, kind, text);
        }
    }
}

impl AgyRunner for RealAgyRunner {
    fn run_turn<'a>(
        &'a self,
        mut request: AgentTurnRequest,
        activity_tx: mpsc::UnboundedSender<AgentActivityUpdate>,
        cancel_rx: watch::Receiver<bool>,
    ) -> TurnFuture<'a> {
        Box::pin(async move {
            request.model_id = sanitize_wildcard_model_id(&request.model_id);
            let has_resumed = request
                .conversation_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some();

            let ctx = self.attempt_context();
            let first = execute_single_attempt(
                &ctx,
                &request,
                &activity_tx,
                cancel_rx.clone(),
                has_resumed,
            )
            .await;

            match first {
                AttemptOutcome::Finished(resp) => Ok(resp),
                AttemptOutcome::Failed(err) => Err(err),
                AttemptOutcome::WatchdogStalled => {
                    let msg = format!(
                        "沿用旧会话超过 {} 秒仍卡在检查点/初始化无实质步骤，已自动终止并无缝切换为全新轻量会话重试...",
                        self.first_progress_watchdog.as_secs().max(1)
                    );
                    self.log_live(&request.slot_id, LiveStreamKind::System, &msg);
                    let _ = activity_tx.send(AgentActivityUpdate {
                        slot_id: request.slot_id.clone(),
                        summary: "原历史会话响应超时，正在自动切换为全新轻量会话重试...".to_string(),
                        tool_steps: 0,
                        conversation_id: None,
                        in_flight_usage: None,
                        peak_context_tokens: 0,
                    });

                    let mut fresh_req = request.clone();
                    fresh_req.conversation_id = None;
                    match execute_single_attempt(&ctx, &fresh_req, &activity_tx, cancel_rx, false)
                        .await
                    {
                        AttemptOutcome::Finished(mut resp) => {
                            resp.switched_to_fresh_session = true;
                            Ok(resp)
                        }
                        AttemptOutcome::Failed(err) => Err(err),
                        AttemptOutcome::WatchdogStalled => Err(AgentTurnError {
                            kind: AgentErrorKind::Timeout,
                            message: "切换全新轻量会话后仍未收到模型首步响应".to_string(),
                            conversation_id: None,
                            partial_usage: None,
                            peak_context_tokens: 0,
                        }),
                    }
                }
            }
        })
    }
}
