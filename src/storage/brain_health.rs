use serde_json::Value;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

pub const MAX_HEALTHY_REUSE_CONTEXT_TOKENS: u64 = 120_000;
pub const TAIL_INSPECT_BYTES: u64 = 16 * 1024;
const MAX_CONV_ID_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptHealth {
    pub context_tokens: u64,
    pub healthy_for_reuse: bool,
    pub unhealthy_reason: Option<String>,
    pub last_step_status: Option<String>,
}

impl Default for TranscriptHealth {
    fn default() -> Self {
        Self {
            context_tokens: 0,
            healthy_for_reuse: true,
            unhealthy_reason: None,
            last_step_status: None,
        }
    }
}

/// 严格校验会话 ID，杜绝路径穿越与非法目录名。
pub fn is_safe_conv_id(conv_id: &str) -> bool {
    if conv_id.is_empty()
        || conv_id.len() > MAX_CONV_ID_LEN
        || conv_id.starts_with('.')
        || conv_id.ends_with('.')
        || conv_id.contains("..")
    {
        return false;
    }
    if !conv_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return false;
    }
    let mut comps = Path::new(conv_id).components();
    matches!(
        (comps.next(), comps.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// 安全解析指定 `conv_id` 对应的 `transcript.jsonl` 路径（校验非符号链接与常规文件）。
pub fn transcript_path_for_conv(brain_dir: &Path, conv_id: &str) -> Option<PathBuf> {
    if !is_safe_conv_id(conv_id) {
        return None;
    }
    let b_meta = fs::symlink_metadata(brain_dir).ok()?;
    if !b_meta.file_type().is_dir() || b_meta.file_type().is_symlink() {
        return None;
    }
    let conv_dir = brain_dir.join(conv_id);
    let c_meta = fs::symlink_metadata(&conv_dir).ok()?;
    if !c_meta.file_type().is_dir() || c_meta.file_type().is_symlink() {
        return None;
    }
    let path = conv_dir
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");
    let t_meta = fs::symlink_metadata(&path).ok()?;
    if !t_meta.file_type().is_file() || t_meta.file_type().is_symlink() {
        return None;
    }
    Some(path)
}

/// 按 `brain_dir + conv_id` 只读检测历史会话尾部健康度（不存在文件时返回 `None`）。
pub fn evaluate_session_health(
    brain_dir: Option<&Path>,
    conv_id: &str,
) -> Option<TranscriptHealth> {
    let dir = brain_dir?;
    let path = transcript_path_for_conv(dir, conv_id)?;
    Some(inspect_transcript_tail_health(&path))
}

/// 只读检查 `transcript.jsonl` 末尾 16KB，提取最近一步上下文水位与收尾健康状态：
/// - `context_tokens = input_tokens + cache_read_tokens > 120_000` -> 标记膨胀不可自动沿用；
/// - 末尾残留 `CANCELED` / `INTERRUPTED` 或连续未收敛 `ERROR_MESSAGE` -> 标记异常不可自动沿用。
pub fn inspect_transcript_tail_health(transcript_path: &Path) -> TranscriptHealth {
    let Ok(mut file) = File::open(transcript_path) else {
        return TranscriptHealth::default();
    };
    let Ok(meta) = file.metadata() else {
        return TranscriptHealth::default();
    };
    let file_len = meta.len();
    if file_len == 0 {
        return TranscriptHealth::default();
    }

    let seek_pos = file_len.saturating_sub(TAIL_INSPECT_BYTES);
    if file.seek(SeekFrom::Start(seek_pos)).is_err() {
        return TranscriptHealth::default();
    }
    let mut raw_bytes = Vec::with_capacity((file_len - seek_pos) as usize);
    if file.read_to_end(&mut raw_bytes).is_err() || raw_bytes.is_empty() {
        return TranscriptHealth::default();
    }

    let slice = if seek_pos > 0 {
        // 跳过可能从中间截断的首行，确保后续均为完整 JSON 行与合法 UTF-8 边界。
        match raw_bytes.iter().position(|&b| b == b'\n') {
            Some(nl) if nl + 1 < raw_bytes.len() => &raw_bytes[nl + 1..],
            _ => &raw_bytes[..],
        }
    } else {
        &raw_bytes[..]
    };

    let decoded = String::from_utf8_lossy(slice);
    let text = decoded.strip_prefix('\u{FEFF}').unwrap_or(&decoded);

    let mut latest_context_tokens: u64 = 0;
    let mut max_context_tokens: u64 = 0;
    let mut last_status: Option<String> = None;
    let mut last_type: Option<String> = None;
    let mut trailing_error_count: u32 = 0;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        if let Some(ctx) = extract_step_context_tokens(&val) {
            latest_context_tokens = ctx;
            max_context_tokens = max_context_tokens.max(ctx);
        }

        let status = val
            .get("status")
            .or_else(|| val.get("step_status"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let step_type = val
            .get("type")
            .or_else(|| val.get("step_type"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);

        if let Some(ref st) = status {
            last_status = Some(st.clone());
        }
        if let Some(ref ty) = step_type {
            last_type = Some(ty.clone());
        }

        let is_error_step = is_error_marker(status.as_deref(), step_type.as_deref());
        if is_error_step {
            trailing_error_count = trailing_error_count.saturating_add(1);
        } else if is_settled_success_step(status.as_deref(), step_type.as_deref()) {
            trailing_error_count = 0;
        }
    }

    let context_tokens = latest_context_tokens.max(max_context_tokens);
    let mut reasons = Vec::new();
    if context_tokens > MAX_HEALTHY_REUSE_CONTEXT_TOKENS {
        reasons.push(format!("上下文已满({}k)", context_tokens / 1000));
    }

    if let Some(ref st) = last_status {
        let upper = st.to_ascii_uppercase();
        if upper.contains("CANCELED") || upper.contains("CANCELLED") || upper.contains("INTERRUPTED")
        {
            reasons.push("上次会话已中断".to_string());
        }
    }

    let tail_is_error = trailing_error_count >= 2
        || (trailing_error_count >= 1
            && last_type
                .as_deref()
                .map(|t| t.to_ascii_uppercase().contains("ERROR"))
                .unwrap_or(false));
    if tail_is_error {
        reasons.push("末尾存在未收敛异常".to_string());
    }

    let healthy_for_reuse = reasons.is_empty();
    let unhealthy_reason = if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("，"))
    };

    TranscriptHealth {
        context_tokens,
        healthy_for_reuse,
        unhealthy_reason,
        last_step_status: last_status,
    }
}

/// 从单条 `transcript.jsonl` 记录提取上下文水位 `input_tokens + cache_read_tokens`。
pub fn extract_step_context_tokens(val: &Value) -> Option<u64> {
    let (inp, cache, _, _) = extract_step_token_quad(val)?;
    let ctx = inp.saturating_add(cache);
    if ctx > 0 {
        Some(ctx)
    } else {
        None
    }
}

/// 从单条 `transcript.jsonl` JSON 对象中提取 `(input, cache_read, output, thinking)` 四元组。
pub fn extract_step_token_quad(val: &Value) -> Option<(u64, u64, u64, u64)> {
    let sources = [
        Some(val),
        val.get("usage"),
        val.get("llm_usage"),
        val.get("token_usage"),
        val.get("metadata").and_then(|m| m.get("usage")),
    ];

    for obj in sources.into_iter().flatten() {
        let Some(map) = obj.as_object() else {
            continue;
        };
        let input = read_u64_any(map, &["input_tokens", "prompt_tokens", "inputTokens"]);
        let cache = read_u64_any(
            map,
            &[
                "cache_read_tokens",
                "cached_content_token_count",
                "cacheReadTokens",
                "cache_read_input_tokens",
            ],
        );
        let output = read_u64_any(map, &["output_tokens", "completion_tokens", "outputTokens"]);
        let thinking = read_u64_any(
            map,
            &["thinking_tokens", "thoughts_token_count", "thinkingTokens"],
        );
        if input > 0 || cache > 0 || output > 0 || thinking > 0 {
            return Some((input, cache, output, thinking));
        }
    }
    None
}

fn read_u64_any(map: &serde_json::Map<String, Value>, keys: &[&str]) -> u64 {
    for k in keys {
        if let Some(v) = map.get(*k).and_then(Value::as_u64) {
            return v;
        }
    }
    0
}

fn is_error_marker(status: Option<&str>, step_type: Option<&str>) -> bool {
    let st_err = status
        .map(|s| {
            let u = s.to_ascii_uppercase();
            u.contains("ERROR") || u.contains("FAILED")
        })
        .unwrap_or(false);
    let ty_err = step_type
        .map(|t| {
            let u = t.to_ascii_uppercase();
            u.contains("ERROR_MESSAGE") || u == "ERROR"
        })
        .unwrap_or(false);
    st_err || ty_err
}

fn is_settled_success_step(status: Option<&str>, step_type: Option<&str>) -> bool {
    let st_ok = status
        .map(|s| {
            let u = s.to_ascii_uppercase();
            u.contains("DONE") || u.contains("SUCCESS") || u.contains("COMPLETED")
        })
        .unwrap_or(false);
    let ty_ok = step_type
        .map(|t| {
            let u = t.to_ascii_uppercase();
            u == "PLANNER_RESPONSE" || u == "AGENT_RESPONSE" || u == "RESULT"
        })
        .unwrap_or(false);
    st_ok || ty_ok
}
