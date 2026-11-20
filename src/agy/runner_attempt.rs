use crate::agy::live_log::{LiveLogSink, LiveStreamKind};
use crate::agy::resolver::ResolvedAgyRuntime;
use crate::agy::runner::{
    AgentActivityUpdate, AgentTurnError, AgentTurnRequest, AgentTurnResponse, RealAgyRunner,
};
use crate::agy::stream_parser::{
    extract_live_stream_lines, extract_structured_json, is_quota_error_text,
    parse_stream_json_line, AgentErrorKind, ParsedStreamEvent, StreamResultPayload, TokenUsage,
};
use crate::agy::usage_tracker::{is_substantive_progress_event, TurnTranscriptUsageTracker};
use crate::win_guard::job_object::SharedJobGuard;
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStderr;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, Duration, MissedTickBehavior};

pub(crate) enum AttemptOutcome {
    Finished(AgentTurnResponse),
    WatchdogStalled,
    Failed(AgentTurnError),
}

pub(crate) struct AttemptContext<'a> {
    pub runtime: &'a ResolvedAgyRuntime,
    pub job_guard: Option<&'a SharedJobGuard>,
    pub live_log: Option<&'a Arc<LiveLogSink>>,
    pub first_progress_watchdog: Duration,
    pub brain_dir_override: Option<PathBuf>,
}

impl<'a> AttemptContext<'a> {
    fn log_live(&self, slot_id: &str, kind: LiveStreamKind, text: impl Into<String>) {
        if let Some(sink) = self.live_log {
            sink.append_for_slot(slot_id, kind, text);
        }
    }
}

pub(crate) async fn execute_single_attempt(
    ctx: &AttemptContext<'_>,
    request: &AgentTurnRequest,
    activity_tx: &mpsc::UnboundedSender<AgentActivityUpdate>,
    mut cancel_rx: watch::Receiver<bool>,
    arm_first_progress_watchdog: bool,
) -> AttemptOutcome {
    let started = Instant::now();
    let turn_timeout = Duration::from_secs(request.timeout_secs.max(30));
    let conv_label = request
        .conversation_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("新开会话");
    ctx.log_live(
        &request.slot_id,
        LiveStreamKind::System,
        format!("启动模型 {}（会话: {conv_label}）", request.model_id),
    );

    let mut usage_tracker = TurnTranscriptUsageTracker::new(
        ctx.brain_dir_override.clone(),
        request.conversation_id.as_deref(),
    );

    let args = RealAgyRunner::build_turn_args(request);
    let mut cmd = ctx.runtime.build_headless_command(&args);
    cmd.current_dir(&request.workspace_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("启动 agy 子进程失败: {e}");
            ctx.log_live(&request.slot_id, LiveStreamKind::Stderr, &msg);
            return fail(AgentErrorKind::ProcessFailed, msg, request.conversation_id.clone(), &usage_tracker);
        }
    };

    if let (Some(guard), Some(pid)) = (ctx.job_guard, child.id()) {
        let _ = guard.assign_pid(pid);
    }

    if let Some(stdin) = child.stdin.take() {
        if let Err(msg) = write_prompt_stdin(stdin, &request.prompt).await {
            return fail(AgentErrorKind::ProcessFailed, msg, request.conversation_id.clone(), &usage_tracker);
        }
    }

    let Some(stdout) = child.stdout.take() else {
        return fail(
            AgentErrorKind::ProcessFailed,
            "无法获取 agy stdout 管道".to_string(),
            request.conversation_id.clone(),
            &usage_tracker,
        );
    };

    let stderr_task = spawn_stderr_collector(
        child.stderr.take(),
        ctx.live_log.cloned(),
        request.slot_id.clone(),
    );

    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut observed_conv_id = request.conversation_id.clone();
    let mut tool_steps: u32 = 0;
    let mut last_summary = String::from("正在思考与分析任务...");
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut final_result: Option<StreamResultPayload> = None;
    let mut saw_substantive_progress = !arm_first_progress_watchdog;

    let mut usage_ticker = interval(Duration::from_millis(1500));
    usage_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let watchdog_sleep = sleep(ctx.first_progress_watchdog);
    tokio::pin!(watchdog_sleep);
    let turn_sleep = sleep(turn_timeout);
    tokio::pin!(turn_sleep);

    loop {
        tokio::select! {
            Ok(()) = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    let _ = child.kill().await;
                    let _ = usage_tracker.poll_incremental();
                    ctx.log_live(&request.slot_id, LiveStreamKind::System, "本轮进程已收到取消信号并终止");
                    return fail(AgentErrorKind::Canceled, "任务已被用户中断".to_string(), observed_conv_id, &usage_tracker);
                }
            }
            _ = &mut turn_sleep => {
                let _ = child.kill().await;
                let _ = usage_tracker.poll_incremental();
                let msg = format!("单轮执行超过 {} 秒超时", turn_timeout.as_secs());
                ctx.log_live(&request.slot_id, LiveStreamKind::Stderr, &msg);
                return fail(AgentErrorKind::Timeout, msg, observed_conv_id, &usage_tracker);
            }
            _ = &mut watchdog_sleep, if !saw_substantive_progress => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return AttemptOutcome::WatchdogStalled;
            }
            _ = usage_ticker.tick() => {
                if let Some((live_u, live_peak)) = usage_tracker.poll_incremental() {
                    let _ = activity_tx.send(AgentActivityUpdate {
                        slot_id: request.slot_id.clone(),
                        summary: last_summary.clone(),
                        tool_steps,
                        conversation_id: observed_conv_id.clone(),
                        in_flight_usage: Some(live_u),
                        peak_context_tokens: live_peak,
                    });
                }
            }
            next = stdout_lines.next_line() => {
                match next {
                    Ok(Some(line)) => {
                        if let Some(sink) = ctx.live_log {
                            for (kind, text) in extract_live_stream_lines(&line) {
                                sink.append_for_slot(&request.slot_id, kind, text);
                            }
                        }
                        if let Some(evt) = parse_stream_json_line(&line) {
                            if !saw_substantive_progress && is_substantive_progress_event(&evt) {
                                saw_substantive_progress = true;
                            }
                            handle_stream_event(
                                evt,
                                &request.slot_id,
                                activity_tx,
                                &mut usage_tracker,
                                &mut observed_conv_id,
                                &mut tool_steps,
                                &mut last_summary,
                                &mut last_emit,
                                &mut final_result,
                            );
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = child.kill().await;
                        let _ = usage_tracker.poll_incremental();
                        return fail(AgentErrorKind::ProcessFailed, format!("读取 agy 输出流失败: {e}"), observed_conv_id, &usage_tracker);
                    }
                }
            }
        }
    }

    let status = match child.wait().await {
        Ok(s) => s,
        Err(e) => {
            let _ = usage_tracker.poll_incremental();
            return fail(AgentErrorKind::ProcessFailed, format!("等待 agy 退出失败: {e}"), observed_conv_id, &usage_tracker);
        }
    };
    let stderr_text = stderr_task.await.unwrap_or_default();
    let _ = usage_tracker.poll_incremental();

    evaluate_attempt_result(
        status,
        stderr_text,
        final_result,
        observed_conv_id,
        tool_steps,
        started,
        &mut usage_tracker,
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_stream_event(
    evt: ParsedStreamEvent,
    slot_id: &str,
    activity_tx: &mpsc::UnboundedSender<AgentActivityUpdate>,
    usage_tracker: &mut TurnTranscriptUsageTracker,
    observed_conv_id: &mut Option<String>,
    tool_steps: &mut u32,
    last_summary: &mut String,
    last_emit: &mut Instant,
    final_result: &mut Option<StreamResultPayload>,
) {
    match evt {
        ParsedStreamEvent::Init {
            conversation_id, ..
        } => {
            if let Some(cid) = conversation_id {
                *observed_conv_id = Some(cid.clone());
                usage_tracker.attach_conversation(&cid);
                let (u_opt, peak) = tracker_opt(usage_tracker);
                *last_summary = "会话已建立，准备分析任务...".to_string();
                let _ = activity_tx.send(AgentActivityUpdate {
                    slot_id: slot_id.to_string(),
                    summary: last_summary.clone(),
                    tool_steps: *tool_steps,
                    conversation_id: Some(cid),
                    in_flight_usage: u_opt,
                    peak_context_tokens: peak,
                });
            }
        }
        ParsedStreamEvent::StepUpdate {
            conversation_id,
            activity_summary,
            is_tool_step,
            state,
            usage,
            ..
        } => {
            if let Some(cid) = conversation_id {
                *observed_conv_id = Some(cid.clone());
                usage_tracker.attach_conversation(&cid);
            }
            if is_tool_step && state.eq_ignore_ascii_case("DONE") {
                *tool_steps += 1;
            }
            let payload_changed = usage
                .as_ref()
                .map(|u| usage_tracker.merge_step_payload_usage(u))
                .unwrap_or(false);
            let file_changed = usage_tracker.poll_incremental().is_some();
            let usage_changed = payload_changed || file_changed;

            if let Some(summary) = activity_summary {
                *last_summary = summary;
            }
            if usage_changed || last_emit.elapsed() >= Duration::from_millis(150) {
                *last_emit = Instant::now();
                let (u_opt, peak) = tracker_snapshot_opt(usage_tracker);
                let _ = activity_tx.send(AgentActivityUpdate {
                    slot_id: slot_id.to_string(),
                    summary: last_summary.clone(),
                    tool_steps: *tool_steps,
                    conversation_id: observed_conv_id.clone(),
                    in_flight_usage: u_opt,
                    peak_context_tokens: peak,
                });
            }
        }
        ParsedStreamEvent::Result(res) => {
            if let Some(cid) = &res.conversation_id {
                *observed_conv_id = Some(cid.clone());
                usage_tracker.attach_conversation(cid);
            }
            *final_result = Some(res);
        }
        ParsedStreamEvent::Ignored => {}
    }
}

fn evaluate_attempt_result(
    status: std::process::ExitStatus,
    stderr_text: String,
    final_result: Option<StreamResultPayload>,
    observed_conv_id: Option<String>,
    tool_steps: u32,
    started: Instant,
    usage_tracker: &mut TurnTranscriptUsageTracker,
) -> AttemptOutcome {
    let Some(res) = final_result else {
        let kind = if is_quota_error_text(&stderr_text) {
            AgentErrorKind::QuotaExhausted
        } else {
            AgentErrorKind::ProcessFailed
        };
        return AttemptOutcome::Failed(make_err(
            kind,
            format!(
                "agy 进程结束 ({status}) 但未返回 result 事件: {}",
                stderr_text.trim()
            ),
            observed_conv_id,
            usage_tracker,
        ));
    };

    let bad_finish = res
        .finished_reason
        .as_deref()
        .map(|r| matches!(r, "error" | "timeout" | "interrupted" | "canceled"))
        .unwrap_or(false);

    if res.status != "SUCCESS" || bad_finish {
        let err_msg = res
            .error
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if !stderr_text.trim().is_empty() {
                    stderr_text.trim().to_string()
                } else {
                    format!("状态异常: {} ({:?})", res.status, res.finished_reason)
                }
            });
        let kind = if is_quota_error_text(&err_msg) || is_quota_error_text(&stderr_text) {
            AgentErrorKind::QuotaExhausted
        } else if res.status == "CANCELED" || res.status == "INTERRUPTED" {
            AgentErrorKind::Canceled
        } else {
            AgentErrorKind::ProcessFailed
        };
        let (final_u, final_peak) = usage_tracker.reconcile_final_usage(&res.usage);
        return AttemptOutcome::Failed(make_err_with_usage(
            kind,
            err_msg,
            observed_conv_id,
            final_u,
            final_peak,
        ));
    }

    let (final_usage, peak_context_tokens) = usage_tracker.reconcile_final_usage(&res.usage);
    let structured_json = match extract_structured_json(&res) {
        Ok(v) => v,
        Err(e) => {
            return AttemptOutcome::Failed(make_err_with_usage(
                AgentErrorKind::InvalidOutput,
                e,
                observed_conv_id,
                final_usage,
                peak_context_tokens,
            ));
        }
    };

    AttemptOutcome::Finished(AgentTurnResponse {
        conversation_id: observed_conv_id,
        structured_json,
        raw_response: res.response,
        duration_seconds: if res.duration_seconds > 0.0 {
            res.duration_seconds
        } else {
            started.elapsed().as_secs_f64()
        },
        tool_steps,
        usage: final_usage,
        peak_context_tokens,
        switched_to_fresh_session: false,
    })
}

fn spawn_stderr_collector(
    stderr: Option<ChildStderr>,
    live_log: Option<Arc<LiveLogSink>>,
    slot_id: String,
) -> JoinHandle<String> {
    tokio::spawn(async move {
        let mut collected = String::new();
        if let Some(err_stream) = stderr {
            let mut reader = BufReader::new(err_stream).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if let Some(sink) = &live_log {
                        sink.append_for_slot(&slot_id, LiveStreamKind::Stderr, trimmed);
                    }
                }
                if collected.len() < 8192 {
                    collected.push_str(&line);
                    collected.push('\n');
                }
            }
        }
        collected
    })
}

async fn write_prompt_stdin(mut stdin: tokio::process::ChildStdin, prompt: &str) -> Result<(), String> {
    let payload = json!({ "event": "user", "message": { "content": prompt } });
    let mut line = serde_json::to_string(&payload).map_err(|e| format!("序列化提示词失败: {e}"))?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await.map_err(|e| format!("写入 agy stdin 失败: {e}"))?;
    let _ = stdin.flush().await;
    let _ = stdin.shutdown().await;
    Ok(())
}

fn tracker_opt(tracker: &mut TurnTranscriptUsageTracker) -> (Option<TokenUsage>, u64) {
    let _ = tracker.poll_incremental();
    tracker_snapshot_opt(tracker)
}

fn tracker_snapshot_opt(tracker: &TurnTranscriptUsageTracker) -> (Option<TokenUsage>, u64) {
    if tracker.has_any_usage() {
        let (u, p) = tracker.current_snapshot();
        (Some(u), p)
    } else {
        (None, 0)
    }
}

fn fail(
    kind: AgentErrorKind,
    message: String,
    conversation_id: Option<String>,
    tracker: &TurnTranscriptUsageTracker,
) -> AttemptOutcome {
    AttemptOutcome::Failed(make_err(kind, message, conversation_id, tracker))
}

fn make_err(
    kind: AgentErrorKind,
    message: String,
    conversation_id: Option<String>,
    tracker: &TurnTranscriptUsageTracker,
) -> AgentTurnError {
    let (u, peak) = tracker.current_snapshot();
    make_err_with_usage(kind, message, conversation_id, u, peak)
}

fn make_err_with_usage(
    kind: AgentErrorKind,
    message: String,
    conversation_id: Option<String>,
    usage: TokenUsage,
    peak_context_tokens: u64,
) -> AgentTurnError {
    AgentTurnError {
        kind,
        message,
        conversation_id,
        partial_usage: if usage.total_tokens > 0 { Some(usage) } else { None },
        peak_context_tokens,
    }
}
