use crate::orchestrator::types::{AgentSlot, RoleKind};
use crate::storage::brain_health::{inspect_transcript_tail_health, is_safe_conv_id};
use crate::storage::brain_parser::{
    collect_json_strings, contains_workspace_at_boundary, detect_role_and_identity,
    extract_architect_from_roster, extract_goal_from_prompt, is_user_prompt_object,
    looks_like_user_input_raw, resolve_brain_identity, unescape_partial_json_line,
};
use crate::storage::workspace_sessions::{WorkspaceRoleSession, SESSION_SOURCE_AGY_BRAIN};
use chrono::{DateTime, Local};
use serde_json::Value;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BRAIN_CONVERSATIONS_SCAN: usize = 80;
const MAX_TRANSCRIPT_SCAN_LINES: usize = 8;
const MAX_TRANSCRIPT_SCAN_BYTES: u64 = 32 * 1024;

pub(crate) fn default_agy_brain_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".gemini").join("antigravity-cli").join("brain"))
}

/// 只读扫描 Antigravity CLI `brain/<conv_id>/.system_generated/logs/transcript.jsonl` 前 8 行（上限 32KB）。
/// 严禁访问任何凭据或非会话日志文件。
pub(super) fn scan_agy_brain_sessions(
    brain_dir: &Path,
    workspace: &str,
    target_key: &str,
    reference_slots: &[AgentSlot],
) -> Vec<WorkspaceRoleSession> {
    if target_key.is_empty() || !brain_dir.exists() {
        return Vec::new();
    }
    let Ok(meta) = fs::symlink_metadata(brain_dir) else {
        return Vec::new();
    };
    if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(brain_dir) else {
        return Vec::new();
    };

    let mut candidates: Vec<(SystemTime, String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let Some(conv_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !is_safe_conv_id(&conv_id) {
            continue;
        }
        let transcript_path = entry
            .path()
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        let Ok(t_meta) = fs::symlink_metadata(&transcript_path) else {
            continue;
        };
        if !t_meta.file_type().is_file() || t_meta.file_type().is_symlink() {
            continue;
        }
        let mtime = t_meta.modified().unwrap_or(UNIX_EPOCH);
        candidates.push((mtime, conv_id, transcript_path));
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    candidates.truncate(MAX_BRAIN_CONVERSATIONS_SCAN);

    let mut discovered = Vec::new();
    for (mtime, conv_id, transcript_path) in candidates {
        if let Some(session) = inspect_transcript_head(
            &transcript_path,
            &conv_id,
            mtime,
            workspace,
            target_key,
            reference_slots,
        ) {
            discovered.push(session);
        }
    }
    discovered
}

fn inspect_transcript_head(
    transcript_path: &Path,
    conv_id: &str,
    mtime: SystemTime,
    workspace: &str,
    target_key: &str,
    reference_slots: &[AgentSlot],
) -> Option<WorkspaceRoleSession> {
    let file = File::open(transcript_path).ok()?;
    let mut buf = Vec::with_capacity(8192);
    file.take(MAX_TRANSCRIPT_SCAN_BYTES)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.is_empty() {
        return None;
    }

    let decoded = decode_utf8_prefix_lossy(&buf);
    let text = decoded.strip_prefix('\u{FEFF}').unwrap_or(&decoded);

    let mut workspace_matched = false;
    let mut detected_role: Option<(RoleKind, Option<String>, Option<String>)> = None;
    let mut extracted_goal = String::new();
    let mut extracted_model = String::new();
    let mut latest_created_at: Option<String> = None;
    let mut roster_arch_meta: Option<(String, String, String)> = None;

    for raw_line in text.lines().take(MAX_TRANSCRIPT_SCAN_LINES) {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
            let mut string_leaves = Vec::new();
            collect_json_strings(&val, 0, &mut string_leaves);

            if !workspace_matched
                && string_leaves
                    .iter()
                    .any(|s| contains_workspace_at_boundary(s, target_key))
            {
                workspace_matched = true;
            }

            if let Some(ts) = val.get("created_at").and_then(Value::as_str) {
                if DateTime::parse_from_rfc3339(ts).is_ok() {
                    latest_created_at = Some(ts.to_string());
                }
            }

            if extracted_model.is_empty() {
                if let Some(m) = val.get("model").and_then(Value::as_str).map(str::trim) {
                    if !m.is_empty() && m != "MODEL_GOOGLE_GEMINI_INTERNAL_BYOM" {
                        extracted_model = m.to_string();
                    }
                }
            }

            if is_user_prompt_object(&val) {
                for leaf in &string_leaves {
                    if detected_role.is_none() {
                        detected_role = detect_role_and_identity(leaf);
                    }
                    if extracted_goal.is_empty() {
                        extracted_goal = extract_goal_from_prompt(leaf);
                    }
                    if roster_arch_meta.is_none() {
                        roster_arch_meta = extract_architect_from_roster(leaf);
                    }
                }
            }
        } else {
            let unescaped = unescape_partial_json_line(trimmed);
            if !workspace_matched && contains_workspace_at_boundary(&unescaped, target_key) {
                workspace_matched = true;
            }
            if detected_role.is_none() && looks_like_user_input_raw(trimmed) {
                detected_role = detect_role_and_identity(&unescaped);
                if extracted_goal.is_empty() {
                    extracted_goal = extract_goal_from_prompt(&unescaped);
                }
            }
        }
    }

    if !workspace_matched {
        return None;
    }
    let (role, parsed_alias, parsed_display) = detected_role?;

    let (slot_id, alias, display_name, model_id) = resolve_brain_identity(
        role,
        parsed_alias,
        parsed_display,
        roster_arch_meta,
        &extracted_model,
        reference_slots,
    );

    let updated_at = latest_created_at.unwrap_or_else(|| {
        if mtime == UNIX_EPOCH {
            String::new()
        } else {
            DateTime::<Local>::from(mtime).to_rfc3339()
        }
    });

    let health = inspect_transcript_tail_health(transcript_path);

    Some(WorkspaceRoleSession {
        workspace: workspace.trim().to_string(),
        workspace_key: target_key.to_string(),
        slot_id,
        alias,
        display_name,
        role,
        model_id,
        conversation_id: conv_id.to_string(),
        last_goal: extracted_goal,
        room_id: None,
        updated_at,
        source: SESSION_SOURCE_AGY_BRAIN.to_string(),
        context_tokens: health.context_tokens,
        healthy_for_reuse: health.healthy_for_reuse,
        unhealthy_reason: health.unhealthy_reason,
    })
}

fn decode_utf8_prefix_lossy(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(err) => {
            let valid_up_to = err.valid_up_to();
            if err.error_len().is_none() {
                String::from_utf8_lossy(&bytes[..valid_up_to]).into_owned()
            } else {
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
    }
}
