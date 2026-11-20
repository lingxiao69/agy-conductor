use crate::agy::stream_parser::{truncate_chars, StreamResultPayload};
use serde_json::Value;

/// 从 `structured_output` 或原始 `response` 文本中稳健提取结构化 JSON 载荷。
/// 当模型正文包含多个代码块或示例对象时，优先从尾部逆向提取最后一个合法 JSON 对象。
pub fn extract_structured_json(result: &StreamResultPayload) -> Result<Value, String> {
    if let Some(val) = &result.structured_output {
        return Ok(val.clone());
    }
    let raw = result.response.trim();
    if raw.is_empty() {
        return Err("模型返回了空响应".to_string());
    }
    if let Ok(val) = serde_json::from_str::<Value>(raw) {
        return Ok(val);
    }

    // 1. 收集所有 Markdown ``` 代码块，优先从尾部逆向匹配合法 JSON 对象。
    let fenced_blocks = collect_markdown_fenced_blocks(raw);
    for block in fenced_blocks.iter().rev() {
        if let Ok(val) = serde_json::from_str::<Value>(block) {
            if val.is_object() {
                return Ok(val);
            }
        }
    }
    for block in fenced_blocks.iter().rev() {
        if let Ok(val) = serde_json::from_str::<Value>(block) {
            return Ok(val);
        }
    }

    // 2. 扫描括号平衡的顶层 `{ ... }` 片段，从尾部逆向尝试解析。
    let balanced_objects = collect_balanced_json_objects(raw);
    for slice in balanced_objects.iter().rev() {
        if let Ok(val) = serde_json::from_str::<Value>(slice) {
            if val.is_object() {
                return Ok(val);
            }
        }
    }

    // 3. 最终兜底：首尾大括号切片。
    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) {
        if start < end {
            if let Ok(val) = serde_json::from_str::<Value>(&raw[start..=end]) {
                return Ok(val);
            }
        }
    }

    Err(format!(
        "无法解析模型结构化输出 JSON: {}",
        truncate_chars(raw, 160)
    ))
}

fn collect_markdown_fenced_blocks(raw: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut cursor = 0usize;
    let bytes = raw.as_bytes();

    while cursor < bytes.len() {
        let Some(rel_open) = raw[cursor..].find("```") else {
            break;
        };
        let open_pos = cursor + rel_open;
        let after_ticks = &raw[open_pos + 3..];
        let Some(nl_rel) = after_ticks.find('\n') else {
            break;
        };
        let body_start = open_pos + 3 + nl_rel + 1;
        let Some(close_rel) = raw[body_start..].find("```") else {
            break;
        };
        let body_end = body_start + close_rel;
        let candidate = raw[body_start..body_end].trim();
        if !candidate.is_empty() {
            blocks.push(candidate);
        }
        cursor = body_end + 3;
    }

    blocks
}

fn collect_balanced_json_objects(raw: &str) -> Vec<&str> {
    let mut objects = Vec::new();
    let mut depth: usize = 0;
    let mut start_idx: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in raw.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                if depth > 0 {
                    in_string = true;
                }
            }
            '{' => {
                if depth == 0 {
                    start_idx = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(start) = start_idx.take() {
                            objects.push(&raw[start..=idx]);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    objects
}

pub fn is_quota_error_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("resource_exhausted")
        || contains_http_429_status(&lower)
        || lower.contains("quota")
        || lower.contains("out of credits")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("no available gemini accounts")
        || lower.contains("no available accounts")
        || lower.contains("account pool exhausted")
        || lower.contains("capacity exhausted")
}

/// 严格按 HTTP 状态码边界匹配 429，防止 `line 429`、`port 429`、`issue #429` 等误判为限流。
fn contains_http_429_status(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut start = 0usize;
    while let Some(rel) = lower[start..].find("429") {
        let pos = start + rel;
        let end = pos + 3;
        start = end;

        let prev_is_alnum = pos > 0 && bytes[pos - 1].is_ascii_alphanumeric();
        let next_is_alnum = end < bytes.len() && bytes[end].is_ascii_alphanumeric();
        if prev_is_alnum || next_is_alnum {
            continue;
        }

        let before = lower[..pos].trim_end();
        let after = lower[end..].trim_start();

        let non_http_prefix = before.ends_with("line")
            || before.ends_with("line:")
            || before.ends_with("col")
            || before.ends_with("column")
            || before.ends_with("port")
            || before.ends_with("pid")
            || before.ends_with('#')
            || before.ends_with("row");
        if non_http_prefix {
            continue;
        }

        let http_prefix = before.ends_with("http")
            || before.ends_with("http/1.1")
            || before.ends_with("http/2")
            || before.ends_with("status")
            || before.ends_with("status:")
            || before.ends_with("status=")
            || before.ends_with("status_code:")
            || before.ends_with("statuscode:")
            || before.ends_with("code")
            || before.ends_with("code:")
            || before.ends_with("code=")
            || before.ends_with("error")
            || before.ends_with("error:")
            || before.ends_with("returned")
            || before.ends_with('(')
            || before.ends_with('[')
            || before.is_empty();

        let http_suffix = after.starts_with("too many")
            || after.starts_with("rate")
            || after.starts_with("quota")
            || after.starts_with("resource_exhausted")
            || after.starts_with("throttl")
            || after.starts_with(')')
            || after.starts_with(']')
            || after.starts_with(':')
            || after.is_empty();

        if http_prefix || http_suffix {
            return true;
        }
    }
    false
}
