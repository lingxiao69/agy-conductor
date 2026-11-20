use crate::agy::runner::{AgentActivityUpdate, AgentTurnError, AgentTurnResponse};
use crate::agy::stream_parser::{
    extract_structured_json, is_quota_error_text, AgentErrorKind, ParsedStreamEvent,
    StreamResultPayload, TokenUsage,
};
use crate::agy::usage_tracker::TurnTranscriptUsageTracker;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::Duration;

pub(crate) enum AttemptOutcome {
    Finished(AgentTurnResponse),
    WatchdogStalled,
    Failed(AgentTurnError),
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_stream_event(
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

pub(crate) fn evaluate_attempt_result(
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

pub(crate) fn tracker_opt(tracker: &mut TurnTranscriptUsageTracker) -> (Option<TokenUsage>, u64) {
    let _ = tracker.poll_incremental();
    tracker_snapshot_opt(tracker)
}

pub(crate) fn tracker_snapshot_opt(tracker: &TurnTranscriptUsageTracker) -> (Option<TokenUsage>, u64) {
    if tracker.has_any_usage() {
        let (u, p) = tracker.current_snapshot();
        (Some(u), p)
    } else {
        (None, 0)
    }
}

pub(crate) fn fail(
    kind: AgentErrorKind,
    message: String,
    conversation_id: Option<String>,
    tracker: &TurnTranscriptUsageTracker,
) -> AttemptOutcome {
    AttemptOutcome::Failed(make_err(kind, message, conversation_id, tracker))
}

pub(crate) fn make_err(
    kind: AgentErrorKind,
    message: String,
    conversation_id: Option<String>,
    tracker: &TurnTranscriptUsageTracker,
) -> AgentTurnError {
    let (u, peak) = tracker.current_snapshot();
    make_err_with_usage(kind, message, conversation_id, u, peak)
}

pub(crate) fn make_err_with_usage(
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
        partial_usage: if usage.total_tokens > 0 {
            Some(usage)
        } else {
            None
        },
        peak_context_tokens,
    }
}
