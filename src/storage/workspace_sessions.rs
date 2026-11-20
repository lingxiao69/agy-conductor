use crate::orchestrator::types::RoleKind;
use crate::storage::brain_scanner::{default_agy_brain_dir, scan_agy_brain_sessions};
use crate::storage::session_matcher::{
    collect_from_history_rooms, collect_from_persisted_config, enrich_candidates_with_tail_health,
    merge_and_dedup_discovered, role_default_slug, sort_sessions_by_recency,
    sort_sessions_by_role_and_recency,
};
use crate::storage::store::{atomic_write_text, AppStorage};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fs;
use std::path::Path;

pub use crate::storage::session_matcher::resolve_reused_sessions_for_slots;

pub const SESSION_SOURCE_CONDUCTOR: &str = "conductor";
pub const SESSION_SOURCE_AGY_BRAIN: &str = "agy_brain";

const MAX_PERSISTED_WORKSPACE_SESSIONS: usize = 600;

fn default_healthy_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRoleSession {
    pub workspace: String,
    #[serde(default)]
    pub workspace_key: String,
    pub slot_id: String,
    pub alias: String,
    pub display_name: String,
    pub role: RoleKind,
    #[serde(default)]
    pub model_id: String,
    pub conversation_id: String,
    #[serde(default)]
    pub last_goal: String,
    #[serde(default)]
    pub room_id: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub context_tokens: u64,
    #[serde(default = "default_healthy_true")]
    pub healthy_for_reuse: bool,
    #[serde(default)]
    pub unhealthy_reason: Option<String>,
}

/// 归一化项目目录键：去除首尾空白、`\` 转 `/`、折叠冗余 `/`、去除末尾 `/`、统一盘符与 Windows 路径小写。
pub fn normalize_workspace_key(workspace: &str) -> String {
    let trimmed = workspace.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let replaced = trimmed.replace('\\', "/");
    let without_verbatim = replaced.strip_prefix("//?/").unwrap_or(&replaced);

    let mut collapsed = String::with_capacity(without_verbatim.len());
    let mut prev_slash = false;
    for (idx, ch) in without_verbatim.chars().enumerate() {
        if ch == '/' {
            if !prev_slash || idx == 1 {
                collapsed.push('/');
            }
            prev_slash = true;
        } else {
            prev_slash = false;
            collapsed.push(ch);
        }
    }

    let stripped = collapsed.trim_end_matches('/');
    let base = if stripped.is_empty() && collapsed.starts_with('/') {
        "/"
    } else {
        stripped
    };

    let is_win_drive = base.as_bytes().get(1) == Some(&b':');
    if cfg!(windows) || is_win_drive {
        base.to_lowercase()
    } else {
        base.to_string()
    }
}

/// 读取全局 `workspace_sessions.json` 中的全部项目角色会话索引。
pub fn load_all_workspace_role_sessions(storage: &AppStorage) -> Vec<WorkspaceRoleSession> {
    let path = storage.base_dir().join("workspace_sessions.json");
    read_sessions_file(&path, true)
}

/// 原子更新或插入单条项目角色会话至全局 `workspace_sessions.json`。
pub fn upsert_workspace_role_session<B>(storage: &AppStorage, session: B) -> Result<(), String>
where
    B: Borrow<WorkspaceRoleSession>,
{
    upsert_workspace_role_sessions(storage, std::iter::once(session.borrow()))
}

/// 批量原子更新或插入项目角色会话至全局 `workspace_sessions.json`。
pub fn upsert_workspace_role_sessions<I, B>(storage: &AppStorage, sessions: I) -> Result<(), String>
where
    I: IntoIterator<Item = B>,
    B: Borrow<WorkspaceRoleSession>,
{
    storage.ensure_dirs()?;
    let path = storage.base_dir().join("workspace_sessions.json");
    let mut all = read_sessions_file(&path, true);

    for raw in sessions {
        let Some(incoming) = sanitize_session(raw.borrow().clone(), SESSION_SOURCE_CONDUCTOR, true)
        else {
            continue;
        };
        upsert_into_vec(&mut all, incoming);
    }

    sort_sessions_by_recency(&mut all);
    if all.len() > MAX_PERSISTED_WORKSPACE_SESSIONS {
        all.truncate(MAX_PERSISTED_WORKSPACE_SESSIONS);
    }

    let json = serde_json::to_string_pretty(&all)
        .map_err(|e| format!("序列化 workspace_sessions.json 失败: {e}"))?;
    atomic_write_text(&path, &json)
}

/// 在 `rooms/<room_id>/sessions.json` 原子保存单房间角色会话快照。
pub fn save_room_role_sessions<I, B>(room_dir: &Path, sessions: I) -> Result<(), String>
where
    I: IntoIterator<Item = B>,
    B: Borrow<WorkspaceRoleSession>,
{
    fs::create_dir_all(room_dir)
        .map_err(|e| format!("确保群组目录存在失败 {}: {e}", room_dir.display()))?;

    let snap = AppStorage::read_room_snapshot(room_dir);
    let mut list: Vec<WorkspaceRoleSession> = Vec::new();
    for raw in sessions {
        let mut s = raw.borrow().clone();
        if let Some(ref sn) = snap {
            if s.workspace.trim().is_empty() {
                s.workspace = sn.workspace.clone();
            }
            if s.last_goal.trim().is_empty() {
                s.last_goal = sn.goal.clone();
            }
            if s.room_id.is_none() && !sn.room_id.trim().is_empty() {
                s.room_id = Some(sn.room_id.clone());
            }
        }
        if let Some(clean) = sanitize_session(s, SESSION_SOURCE_CONDUCTOR, false) {
            upsert_into_vec(&mut list, clean);
        }
    }
    sort_sessions_by_role_and_recency(&mut list);

    let path = room_dir.join("sessions.json");
    let json = serde_json::to_string_pretty(&list)
        .map_err(|e| format!("序列化房间 sessions.json 失败: {e}"))?;
    atomic_write_text(&path, &json)
}

/// 读取 `rooms/<room_id>/sessions.json` 单房间角色会话快照。
pub fn read_room_role_sessions(room_dir: &Path) -> Vec<WorkspaceRoleSession> {
    read_sessions_file(&room_dir.join("sessions.json"), false)
}

/// 自动发现并融合目标 `workspace` 下的全部历史身份角色会话：
/// `workspace_sessions.json` + `rooms/*/sessions.json` + `config.json` + 本机 `brain` 历史日志，
/// 并通过 `brain_health` 检查每个会话的尾部上下文水位与收尾健康状态。
pub fn discover_workspace_role_sessions(
    storage: &AppStorage,
    workspace: &str,
    agy_brain_dir_override: Option<&Path>,
) -> Vec<WorkspaceRoleSession> {
    let target_key = normalize_workspace_key(workspace);
    if target_key.is_empty() {
        return Vec::new();
    }

    let cfg = storage.load_config();
    let mut candidates: Vec<WorkspaceRoleSession> = load_all_workspace_role_sessions(storage)
        .into_iter()
        .filter(|item| item.workspace_key == target_key)
        .collect();

    collect_from_history_rooms(storage, workspace, &target_key, &mut candidates);

    let brain_dir = agy_brain_dir_override
        .map(Path::to_path_buf)
        .or_else(default_agy_brain_dir);
    let brain_sessions = match brain_dir {
        Some(ref dir) => scan_agy_brain_sessions(dir, workspace, &target_key, &cfg.slots),
        None => Vec::new(),
    };

    collect_from_persisted_config(
        storage,
        &cfg,
        workspace,
        &target_key,
        &brain_sessions,
        &mut candidates,
    );
    candidates.extend(brain_sessions);
    enrich_candidates_with_tail_health(&mut candidates, brain_dir.as_deref());

    merge_and_dedup_discovered(candidates)
}

fn read_sessions_file(path: &Path, require_workspace: bool) -> Vec<WorkspaceRoleSession> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let trimmed = raw.trim_start_matches('\u{FEFF}').trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(parsed) = serde_json::from_str::<Vec<WorkspaceRoleSession>>(trimmed) else {
        return Vec::new();
    };
    parsed
        .into_iter()
        .filter_map(|s| sanitize_session(s, SESSION_SOURCE_CONDUCTOR, require_workspace))
        .collect()
}

pub(super) fn sanitize_session(
    mut s: WorkspaceRoleSession,
    default_source: &str,
    require_workspace: bool,
) -> Option<WorkspaceRoleSession> {
    s.workspace = s.workspace.trim().to_string();
    s.conversation_id = s.conversation_id.trim().to_string();
    if s.conversation_id.is_empty() {
        return None;
    }
    s.workspace_key = if !s.workspace.is_empty() {
        normalize_workspace_key(&s.workspace)
    } else {
        normalize_workspace_key(&s.workspace_key)
    };
    if require_workspace && s.workspace_key.is_empty() {
        return None;
    }

    s.slot_id = s.slot_id.trim().to_string();
    s.alias = s.alias.trim().trim_start_matches('@').to_string();
    if s.slot_id.is_empty() && !s.alias.is_empty() {
        s.slot_id = s.alias.clone();
    } else if s.alias.is_empty() && !s.slot_id.is_empty() {
        s.alias = s.slot_id.clone();
    }
    if s.slot_id.is_empty() {
        let slug = role_default_slug(s.role);
        s.slot_id = slug.to_string();
        s.alias = slug.to_string();
    }

    s.display_name = s.display_name.trim().to_string();
    if s.display_name.is_empty() {
        s.display_name = format!("{} (@{})", s.role.label_zh(), s.alias);
    }
    s.model_id = s.model_id.trim().to_string();
    s.last_goal = s.last_goal.trim().to_string();
    s.room_id = s
        .room_id
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());
    s.updated_at = s.updated_at.trim().to_string();
    if s.updated_at.is_empty() {
        s.updated_at = Local::now().to_rfc3339();
    }
    s.source = s.source.trim().to_string();
    if s.source.is_empty() {
        s.source = default_source.to_string();
    }
    Some(s)
}

fn upsert_into_vec(list: &mut Vec<WorkspaceRoleSession>, mut incoming: WorkspaceRoleSession) {
    let mut matched_prev: Option<WorkspaceRoleSession> = None;
    list.retain(|existing| {
        let same_ws = existing.workspace_key == incoming.workspace_key;
        let same_slot = same_ws
            && existing.role == incoming.role
            && existing.slot_id.eq_ignore_ascii_case(&incoming.slot_id);
        let same_conv = same_ws && existing.conversation_id == incoming.conversation_id;
        if same_slot || same_conv {
            if existing.conversation_id == incoming.conversation_id {
                matched_prev = Some(existing.clone());
            }
            false
        } else {
            true
        }
    });

    if let Some(prev) = matched_prev {
        if incoming.last_goal.is_empty() && !prev.last_goal.is_empty() {
            incoming.last_goal = prev.last_goal;
        }
        if incoming.room_id.is_none() && prev.room_id.is_some() {
            incoming.room_id = prev.room_id;
        }
        if incoming.model_id.is_empty() && !prev.model_id.is_empty() {
            incoming.model_id = prev.model_id;
        }
        if incoming.context_tokens == 0 && prev.context_tokens > 0 {
            incoming.context_tokens = prev.context_tokens;
        }
    }
    list.push(incoming);
}
