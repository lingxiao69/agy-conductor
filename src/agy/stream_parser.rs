pub use crate::agy::json_extract::{extract_structured_json, is_quota_error_text};
use crate::agy::live_log::LiveStreamKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub thinking_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentErrorKind {
    QuotaExhausted,
    Timeout,
    Canceled,
    InvalidOutput,
    ProcessFailed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedStreamEvent {
    Init {
        conversation_id: Option<String>,
        model: Option<String>,
    },
    StepUpdate {
        conversation_id: Option<String>,
        step_type: String,
        state: String,
        activity_summary: Option<String>,
        is_tool_step: bool,
        has_thinking: bool,
        has_text: bool,
        usage: Option<TokenUsage>,
    },
    Result(StreamResultPayload),
    Ignored,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamResultPayload {
    pub conversation_id: Option<String>,
    pub status: String,
    pub response: String,
    pub structured_output: Option<Value>,
    pub error: Option<String>,
    pub finished_reason: Option<String>,
    pub duration_seconds: f64,
    pub usage: TokenUsage,
}

pub fn parse_stream_json_line(line: &str) -> Option<ParsedStreamEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }
    let root: Value = serde_json::from_str(trimmed).ok()?;
    let event_type = root.get("event").and_then(|v| v.as_str())?;

    match event_type {
        "init" => {
            let conversation_id = root
                .get("conversation_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            let model = root
                .get("init")
                .and_then(|v| v.get("model"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            Some(ParsedStreamEvent::Init {
                conversation_id,
                model,
            })
        }
        "step_update" => {
            let payload = root.get("step_update")?;
            let conversation_id = payload
                .get("conversation_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            let step_type = payload
                .get("step_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let state = payload
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let (activity_summary, is_tool_step) = summarize_step_update(payload, &step_type);
            let has_thinking = payload
                .get("thinking")
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            let has_text = payload
                .get("text")
                .or_else(|| payload.get("output"))
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            let usage = payload
                .get("usage")
                .and_then(|u| serde_json::from_value::<TokenUsage>(u.clone()).ok())
                .filter(|u| {
                    u.input_tokens > 0
                        || u.output_tokens > 0
                        || u.thinking_tokens > 0
                        || u.cache_read_tokens > 0
                        || u.total_tokens > 0
                });
            Some(ParsedStreamEvent::StepUpdate {
                conversation_id,
                step_type,
                state,
                activity_summary,
                is_tool_step,
                has_thinking,
                has_text,
                usage,
            })
        }
        "result" => {
            let res = root.get("result")?;
            let conversation_id = res
                .get("conversation_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            let status = res
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("UNKNOWN")
                .to_string();
            let response = res
                .get("response")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let structured_output = res
                .get("structured_output")
                .cloned()
                .filter(|v| !v.is_null());
            let error = res
                .get("error")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            let finished_reason = res
                .get("finished_reason")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            let duration_seconds = res
                .get("duration_seconds")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let usage = res
                .get("usage")
                .and_then(|u| serde_json::from_value::<TokenUsage>(u.clone()).ok())
                .unwrap_or_default();

            Some(ParsedStreamEvent::Result(StreamResultPayload {
                conversation_id,
                status,
                response,
                structured_output,
                error,
                finished_reason,
                duration_seconds,
                usage,
            }))
        }
        _ => Some(ParsedStreamEvent::Ignored),
    }
}

/// 从 `agy` 单行 `stream-json` 或普通输出中提取供实时终端流展示的明细行。
pub fn extract_live_stream_lines(line: &str) -> Vec<(LiveStreamKind, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if !trimmed.starts_with('{') {
        return vec![(LiveStreamKind::Output, truncate_chars(trimmed, 240))];
    }
    let Ok(root) = serde_json::from_str::<Value>(trimmed) else {
        return vec![(LiveStreamKind::Output, truncate_chars(trimmed, 240))];
    };
    let Some(event_type) = root.get("event").and_then(Value::as_str) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    match event_type {
        "init" => {
            let cid = root
                .get("conversation_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let model = root
                .get("init")
                .and_then(|v| v.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("-");
            out.push((
                LiveStreamKind::System,
                format!("会话已建立 · model={model} · conv={cid}"),
            ));
        }
        "step_update" => {
            if let Some(payload) = root.get("step_update") {
                let step_type = payload
                    .get("step_type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let state = payload
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let before_len = out.len();
                if let Some(thinking) = payload.get("thinking").and_then(Value::as_str) {
                    let clean = collapse_whitespace(thinking);
                    if !clean.is_empty() {
                        out.push((LiveStreamKind::Thinking, truncate_chars(&clean, 220)));
                    }
                }
                if let Some(tool_info) = payload.get("tool_info") {
                    let tool_name = tool_info
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| payload.get("tool_name").and_then(Value::as_str))
                        .unwrap_or("tool");
                    let target = extract_tool_target(tool_info.get("parameters"));
                    let detail = match target {
                        Some(t) => format!("{tool_name} -> {t}"),
                        None => tool_name.to_string(),
                    };
                    let suffix = if state.is_empty() {
                        String::new()
                    } else {
                        format!(" [{state}]")
                    };
                    out.push((LiveStreamKind::ToolCall, format!("{detail}{suffix}")));
                } else if let Some(tool_name) = payload.get("tool_name").and_then(Value::as_str) {
                    out.push((LiveStreamKind::ToolCall, format!("{tool_name} [{state}]")));
                }
                let mut emitted_text = false;
                if let Some(text) = payload
                    .get("text")
                    .or_else(|| payload.get("output"))
                    .and_then(Value::as_str)
                {
                    let clean = collapse_whitespace(text);
                    if !clean.is_empty() {
                        emitted_text = true;
                        out.push((LiveStreamKind::Output, truncate_chars(&clean, 220)));
                    }
                }
                if out.len() == before_len {
                    let suffix = if state.is_empty() {
                        String::new()
                    } else {
                        format!(" [{state}]")
                    };
                    if step_type.eq_ignore_ascii_case("checkpoint") {
                        out.push((
                            LiveStreamKind::System,
                            format!("正在压缩与保存会话检查点...{suffix}"),
                        ));
                    } else if step_type.eq_ignore_ascii_case("agent_response") && !emitted_text {
                        out.push((
                            LiveStreamKind::System,
                            format!("正在生成结构化派单/汇报 JSON...{suffix}"),
                        ));
                    }
                }
            }
        }
        "result" => {
            if let Some(res) = root.get("result") {
                let status = res
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("UNKNOWN");
                let dur = res
                    .get("duration_seconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                if let Some(err) = res.get("error").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    out.push((
                        LiveStreamKind::Stderr,
                        format!("本轮异常 ({status}, {dur:.1}s): {}", truncate_chars(err, 220)),
                    ));
                } else {
                    let chat_preview = res
                        .get("structured_output")
                        .and_then(|v| v.get("chat_message"))
                        .and_then(Value::as_str)
                        .or_else(|| res.get("response").and_then(Value::as_str))
                        .unwrap_or("");
                    let preview = truncate_chars(&collapse_whitespace(chat_preview), 180);
                    let line_text = if preview.is_empty() {
                        format!("本轮完成 · status={status} · 耗时 {dur:.1}s")
                    } else {
                        format!("本轮完成 ({status}, {dur:.1}s): {preview}")
                    };
                    out.push((LiveStreamKind::Result, line_text));
                }
            }
        }
        _ => {}
    }
    out
}

fn collapse_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn summarize_step_update(payload: &Value, step_type: &str) -> (Option<String>, bool) {
    if let Some(tool_info) = payload.get("tool_info") {
        let tool_name = tool_info
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("tool_name").and_then(|v| v.as_str()))
            .unwrap_or("tool");
        let target = extract_tool_target(tool_info.get("parameters"));
        let summary = match (tool_name, target) {
            ("read_file" | "view_file", Some(t)) => format!("正在阅读 {t}"),
            ("edit_file" | "replace_file_content" | "multi_replace_file_content", Some(t)) => {
                format!("正在修改 {t}")
            }
            ("write_file" | "write_to_file", Some(t)) => format!("正在写入 {t}"),
            ("run_command", Some(t)) => format!("正在执行命令: {t}"),
            ("grep_search" | "find_by_name" | "list_dir", Some(t)) => {
                format!("正在检索 {t}")
            }
            (name, Some(t)) => format!("正在调用 {name} ({t})"),
            (name, None) => format!("正在调用 {name}..."),
        };
        return (Some(summary), true);
    }

    if let Some(tool_name) = payload.get("tool_name").and_then(|v| v.as_str()) {
        return (Some(format!("正在调用 {tool_name}...")), true);
    }

    if let Some(thinking) = payload.get("thinking").and_then(|v| v.as_str()) {
        let brief = truncate_chars(thinking.trim(), 48);
        if !brief.is_empty() {
            return (Some(format!("思考中: {brief}")), false);
        }
    }

    match step_type {
        "agent_response" => (Some("正在组织结构化汇报...".to_string()), false),
        "checkpoint" => (Some("正在保存会话检查点...".to_string()), false),
        _ => (None, false),
    }
}

fn extract_tool_target(params: Option<&Value>) -> Option<String> {
    let obj = params?.as_object()?;
    let keys = [
        "Path",
        "path",
        "TargetFile",
        "AbsolutePath",
        "DirectoryPath",
        "SearchPath",
        "CommandLine",
        "Query",
        "Pattern",
    ];
    for key in keys {
        if let Some(val) = obj.get(key).and_then(|v| v.as_str()) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(shorten_path_or_cmd(trimmed));
            }
        }
    }
    None
}

fn shorten_path_or_cmd(raw: &str) -> String {
    let single_line = raw.lines().next().unwrap_or(raw).trim();
    if single_line.contains('\\') || single_line.contains('/') {
        let normalized = single_line.replace('\\', "/");
        let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() >= 2 {
            return format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]);
        }
    }
    truncate_chars(single_line, 44)
}

pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in s.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}
