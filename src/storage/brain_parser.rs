use crate::orchestrator::types::{AgentSlot, RoleKind};
use serde_json::Value;

const MARKER_ARCHITECT: &str = "【总架构规划师 (Architect)】";
const MARKER_CODER: &str = "【编码程序员 (Coder)】";
const MARKER_REVIEWER: &str = "你是专职代码审查员";

pub(super) fn collect_json_strings(val: &Value, depth: usize, out: &mut Vec<String>) {
    if depth > 6 {
        return;
    }
    match val {
        Value::String(s) => {
            out.push(s.clone());
            let trimmed = s.trim();
            if depth < 3
                && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
                    || (trimmed.starts_with('{') && trimmed.ends_with('}'))
                    || (trimmed.starts_with('[') && trimmed.ends_with(']')))
            {
                if let Ok(nested) = serde_json::from_str::<Value>(trimmed) {
                    collect_json_strings(&nested, depth + 1, out);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_json_strings(item, depth + 1, out);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_json_strings(v, depth + 1, out);
            }
        }
        _ => {}
    }
}

pub(super) fn is_user_prompt_object(val: &Value) -> bool {
    let step_type = val.get("type").and_then(Value::as_str);
    let source = val.get("source").and_then(Value::as_str);
    let role = val.get("role").and_then(Value::as_str);

    if let Some(t) = step_type {
        return t.eq_ignore_ascii_case("USER_INPUT") || t.eq_ignore_ascii_case("user");
    }
    if let Some(r) = role {
        return r.eq_ignore_ascii_case("user");
    }
    if let Some(src) = source {
        return src.eq_ignore_ascii_case("USER_EXPLICIT") || src.eq_ignore_ascii_case("USER");
    }
    val.get("content").is_some() || val.get("prompt").is_some() || val.get("text").is_some()
}

pub(super) fn looks_like_user_input_raw(raw_line: &str) -> bool {
    !raw_line.contains("\"type\":\"GENERIC\"")
        && !raw_line.contains("\"type\":\"PLANNER_RESPONSE\"")
}

/// 按最早出现的角色声明位置识别身份，避免提示词后半段架构契约引用其他角色文本造成误判。
pub(super) fn detect_role_and_identity(
    text: &str,
) -> Option<(RoleKind, Option<String>, Option<String>)> {
    let candidates = [
        text.find(MARKER_ARCHITECT)
            .map(|idx| (idx, RoleKind::Architect)),
        text.find(MARKER_CODER).map(|idx| (idx, RoleKind::Coder)),
        text.find(MARKER_REVIEWER)
            .map(|idx| (idx, RoleKind::Reviewer)),
    ];
    let (pos, role) = candidates.into_iter().flatten().min_by_key(|(idx, _)| *idx)?;

    match role {
        RoleKind::Coder => {
            let after = &text[pos + MARKER_CODER.len()..];
            let header_line = after.lines().next().unwrap_or("");
            let alias = extract_first_mention_alias(header_line);
            let display = extract_coder_display_name(header_line);
            Some((RoleKind::Coder, alias, display))
        }
        RoleKind::Reviewer => {
            let after = &text[pos + MARKER_REVIEWER.len()..];
            let header_line = after.lines().next().unwrap_or("");
            let alias = extract_first_mention_alias(header_line);
            Some((RoleKind::Reviewer, alias, None))
        }
        RoleKind::Architect => {
            let after = &text[pos + MARKER_ARCHITECT.len()..];
            let header_line = after.lines().next().unwrap_or("");
            let alias = extract_first_mention_alias(header_line);
            Some((RoleKind::Architect, alias, None))
        }
    }
}

fn extract_coder_display_name(header_line: &str) -> Option<String> {
    let trimmed = header_line
        .trim_start()
        .strip_prefix('：')
        .or_else(|| header_line.trim_start().strip_prefix(':'))?
        .trim();
    let name_part = trimmed
        .split(['(', '（'])
        .next()
        .unwrap_or(trimmed)
        .trim();
    if name_part.is_empty() || name_part.starts_with('@') {
        None
    } else {
        Some(name_part.to_string())
    }
}

fn extract_first_mention_alias(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b'_'))
            {
                end += 1;
            }
            if end > start {
                return Some(line[start..end].to_string());
            }
        }
        i += 1;
    }
    None
}

pub(super) fn extract_architect_from_roster(text: &str) -> Option<(String, String, String)> {
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("- @") || !trimmed.contains("角色: 总架构师") {
            continue;
        }
        let parts: Vec<&str> = trimmed
            .trim_start_matches("- ")
            .split('|')
            .map(str::trim)
            .collect();
        if parts.len() >= 5 {
            let alias = parts[0].trim_start_matches('@').trim().to_string();
            let display = parts[1].to_string();
            let model = parts[4].strip_prefix("模型:").unwrap_or("").trim().to_string();
            if !alias.is_empty() {
                return Some((alias, display, model));
            }
        }
    }
    None
}

pub(super) fn extract_goal_from_prompt(text: &str) -> String {
    for heading in ["## 项目长期目标", "## 用户长期目标", "## 原始长期目标"] {
        if let Some(idx) = text.find(heading) {
            let rest = &text[idx + heading.len()..];
            let mut lines = Vec::new();
            for line in rest.lines().skip(1) {
                let t = line.trim();
                if t.starts_with("## ")
                    || t.starts_with("</USER_REQUEST>")
                    || t.starts_with("<truncated")
                {
                    break;
                }
                if !t.is_empty() {
                    lines.push(t);
                    if lines.len() >= 2 {
                        break;
                    }
                }
            }
            if !lines.is_empty() {
                let joined = lines.join(" ");
                return if joined.chars().count() <= 120 {
                    joined
                } else {
                    format!("{}…", joined.chars().take(120).collect::<String>())
                };
            }
        }
    }
    String::new()
}

pub(super) fn resolve_brain_identity(
    role: RoleKind,
    parsed_alias: Option<String>,
    parsed_display: Option<String>,
    roster_arch_meta: Option<(String, String, String)>,
    extracted_model: &str,
    reference_slots: &[AgentSlot],
) -> (String, String, String, String) {
    let mut alias = parsed_alias.unwrap_or_default();
    let mut display_name = parsed_display.unwrap_or_default();
    let mut model_id = extracted_model.to_string();

    if role == RoleKind::Architect {
        if let Some((r_alias, r_disp, r_model)) = roster_arch_meta {
            if alias.is_empty() {
                alias = r_alias;
            }
            if display_name.is_empty() {
                display_name = r_disp;
            }
            if model_id.is_empty() && !r_model.is_empty() {
                model_id = r_model;
            }
        }
    }

    let matched_slot = if !alias.is_empty() {
        reference_slots
            .iter()
            .find(|s| s.role == role && s.alias.eq_ignore_ascii_case(&alias))
    } else if role != RoleKind::Coder {
        let mut role_slots = reference_slots.iter().filter(|s| s.role == role);
        let first = role_slots.next();
        if role_slots.next().is_none() {
            first
        } else {
            None
        }
    } else {
        None
    };

    if let Some(slot) = matched_slot {
        let final_alias = if alias.is_empty() {
            slot.alias.clone()
        } else {
            alias
        };
        let final_disp = if display_name.is_empty() {
            slot.display_name.clone()
        } else {
            display_name
        };
        let final_model = if model_id.is_empty() {
            slot.model_id.clone()
        } else {
            model_id
        };
        return (slot.slot_id.clone(), final_alias, final_disp, final_model);
    }

    let fallback_slug = match role {
        RoleKind::Architect => "architect",
        RoleKind::Coder => "coder",
        RoleKind::Reviewer => "reviewer",
    };
    let final_alias = if alias.is_empty() {
        fallback_slug.to_string()
    } else {
        alias
    };
    let final_disp = if !display_name.is_empty() {
        display_name
    } else if final_alias != fallback_slug {
        format!("{} · @{final_alias}", role.label_zh())
    } else {
        role.label_zh().to_string()
    };
    (final_alias.clone(), final_alias, final_disp, model_id)
}

pub(super) fn contains_workspace_at_boundary(raw_text: &str, target_key: &str) -> bool {
    if target_key.is_empty() || raw_text.is_empty() {
        return false;
    }
    let fold_case = cfg!(windows) || target_key.as_bytes().get(1) == Some(&b':');
    let hay = normalize_text_for_path_search(raw_text, fold_case);
    let mut search_from = 0;
    while let Some(rel) = hay[search_from..].find(target_key) {
        let start = search_from + rel;
        let end = start + target_key.len();

        let prev_ok = if start == 0 {
            true
        } else {
            hay[..start]
                .chars()
                .next_back()
                .map(|c| !is_path_segment_char(c) && c != '/')
                .unwrap_or(true)
        };

        let next_ok = if end >= hay.len() {
            true
        } else {
            hay[end..]
                .chars()
                .next()
                .map(|c| c == '/' || !is_path_segment_char(c))
                .unwrap_or(true)
        };

        if prev_ok && next_ok {
            return true;
        }
        search_from = start + 1;
        while search_from < hay.len() && !hay.is_char_boundary(search_from) {
            search_from += 1;
        }
    }
    false
}

fn normalize_text_for_path_search(raw: &str, fold_case: bool) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_slash = false;
    for ch in raw.chars() {
        let mapped = if ch == '\\' { '/' } else { ch };
        if mapped == '/' {
            if !prev_slash {
                out.push('/');
            }
            prev_slash = true;
        } else {
            prev_slash = false;
            if fold_case {
                for lc in mapped.to_lowercase() {
                    out.push(lc);
                }
            } else {
                out.push(mapped);
            }
        }
    }
    out.replace("/?/", "")
}

fn is_path_segment_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

pub(super) fn unescape_partial_json_line(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek().copied() {
                Some('\\') => {
                    chars.next();
                    out.push('\\');
                }
                Some('n') => {
                    chars.next();
                    out.push('\n');
                }
                Some('r') => {
                    chars.next();
                    out.push('\r');
                }
                Some('t') => {
                    chars.next();
                    out.push('\t');
                }
                Some('"') => {
                    chars.next();
                    out.push('"');
                }
                Some('/') => {
                    chars.next();
                    out.push('/');
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
