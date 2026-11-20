use crate::agy::live_log::{LiveLogSink, LiveStreamKind};
use crate::agy::resolver::ResolvedAgyRuntime;
use crate::agy::runner::{AgentActivityUpdate, AgentTurnRequest, RealAgyRunner};
pub(crate) use crate::agy::runner_eval::AttemptOutcome;
use crate::agy::runner_eval::{evaluate_attempt_result, fail, handle_stream_event};
use crate::agy::stream_parser::{
    extract_live_stream_lines, parse_stream_json_line, AgentErrorKind, StreamResultPayload,
};
use crate::agy::usage_tracker::{is_substantive_progress_event, TurnTranscriptUsageTracker};
use crate::win_guard::job_object::SharedJobGuard;
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{interval, sleep, timeout, Duration, MissedTickBehavior};

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

async fn reap_child_only(child: &mut Child) {
    let _ = child.kill().await;
    let _ = timeout(Duration::from_millis(1500), child.wait()).await;
}

async fn reap_child_and_stderr(child: &mut Child, stderr_task: JoinHandle<String>) {
    reap_child_only(child).await;
    stderr_task.abort();
    let _ = timeout(Duration::from_millis(300), stderr_task).await;
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
            return fail(
                AgentErrorKind::ProcessFailed,
                msg,
                request.conversation_id.clone(),
                &usage_tracker,
            );
        }
    };

    if let (Some(guard), Some(pid)) = (ctx.job_guard, child.id()) {
        let _ = guard.assign_pid(pid);
    }

    if let Some(stdin) = child.stdin.take() {
        if let Err(msg) = write_prompt_stdin(stdin, &request.prompt).await {
            reap_child_only(&mut child).await;
            return fail(
                AgentErrorKind::ProcessFailed,
                msg,
                request.conversation_id.clone(),
                &usage_tracker,
            );
        }
    }

    let Some(stdout) = child.stdout.take() else {
        reap_child_only(&mut child).await;
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
                    reap_child_and_stderr(&mut child, stderr_task).await;
                    let _ = usage_tracker.poll_incremental();
                    ctx.log_live(&request.slot_id, LiveStreamKind::System, "本轮进程已收到取消信号并终止");
                    return fail(AgentErrorKind::Canceled, "任务已被用户中断".to_string(), observed_conv_id, &usage_tracker);
                }
            }
            _ = &mut turn_sleep => {
                reap_child_and_stderr(&mut child, stderr_task).await;
                let _ = usage_tracker.poll_incremental();
                let msg = format!("单轮执行超过 {} 秒超时", turn_timeout.as_secs());
                ctx.log_live(&request.slot_id, LiveStreamKind::Stderr, &msg);
                return fail(AgentErrorKind::Timeout, msg, observed_conv_id, &usage_tracker);
            }
            _ = &mut watchdog_sleep, if !saw_substantive_progress => {
                reap_child_and_stderr(&mut child, stderr_task).await;
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
                        reap_child_and_stderr(&mut child, stderr_task).await;
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
            stderr_task.abort();
            let _ = timeout(Duration::from_millis(300), stderr_task).await;
            let _ = usage_tracker.poll_incremental();
            return fail(
                AgentErrorKind::ProcessFailed,
                format!("等待 agy 退出失败: {e}"),
                observed_conv_id,
                &usage_tracker,
            );
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

async fn write_prompt_stdin(
    mut stdin: tokio::process::ChildStdin,
    prompt: &str,
) -> Result<(), String> {
    let payload = json!({ "event": "user", "message": { "content": prompt } });
    let mut line =
        serde_json::to_string(&payload).map_err(|e| format!("序列化提示词失败: {e}"))?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("写入 agy stdin 失败: {e}"))?;
    let _ = stdin.flush().await;
    let _ = stdin.shutdown().await;
    Ok(())
}
