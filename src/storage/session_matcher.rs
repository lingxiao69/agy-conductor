use crate::orchestrator::types::{AgentSlot, RoleKind};
use crate::storage::brain_health::evaluate_session_health;
use crate::storage::history::validate_room_id;
use crate::storage::store::{AppStorage, PersistedConfig};
use crate::storage::workspace_sessions::{
    normalize_workspace_key, sanitize_session, WorkspaceRoleSession, SESSION_SOURCE_CONDUCTOR,
};
use chrono::{DateTime, Local};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// 为当前编队槽位智能匹配可沿用的历史角色会话：
/// 1. `explicit_map`（`slot_id -> conversation_id`）最高优先级；若显式值为空或 `"__new__"` / `"new"` / `"none"` 则跳过沿用；
/// 2. 自动匹配阶段（Pass 2~4）**严格仅绑定 `healthy_for_reuse == true` 的健康轻量会话**，
///    绝不自动强塞 `> 120k` 膨胀或末尾中断/异常的旧会话！
pub fn resolve_reused_sessions_for_slots(
    slots: &[AgentSlot],
    available: &[WorkspaceRoleSession],
    explicit_map: &HashMap<String, String>,
) -> HashMap<String, WorkspaceRoleSession> {
    let mut sorted_available = available.to_vec();
    sort_sessions_by_recency(&mut sorted_available);

    let mut matched: HashMap<String, WorkspaceRoleSession> = HashMap::new();
    let mut skipped_slots: HashSet<String> = HashSet::new();
    let mut used_conversations: HashSet<String> = HashSet::new();

    // Pass 1: 显式 slot_id -> conversation_id 映射优先（用户手动指定）
    for slot in slots {
        let Some(raw_cid) = explicit_map.get(&slot.slot_id) else {
            continue;
        };
        let cid = raw_cid.trim();
        if cid.is_empty()
            || cid.eq_ignore_ascii_case("new")
            || cid.eq_ignore_ascii_case("__new__")
            || cid.eq_ignore_ascii_case("none")
        {
            skipped_slots.insert(slot.slot_id.clone());
            continue;
        }
        if let Some(found) = sorted_available.iter().find(|s| s.conversation_id == cid) {
            used_conversations.insert(found.conversation_id.clone());
            matched.insert(slot.slot_id.clone(), found.clone());
        } else {
            used_conversations.insert(cid.to_string());
            matched.insert(
                slot.slot_id.clone(),
                WorkspaceRoleSession {
                    workspace: String::new(),
                    workspace_key: String::new(),
                    slot_id: slot.slot_id.clone(),
                    alias: slot.alias.clone(),
                    display_name: slot.display_name.clone(),
                    role: slot.role,
                    model_id: slot.model_id.clone(),
                    conversation_id: cid.to_string(),
                    last_goal: String::new(),
                    room_id: None,
                    updated_at: String::new(),
                    source: SESSION_SOURCE_CONDUCTOR.to_string(),
                    context_tokens: 0,
                    healthy_for_reuse: true,
                    unhealthy_reason: None,
                },
            );
        }
    }

    let mut ordered_slots: Vec<&AgentSlot> = slots.iter().collect();
    ordered_slots.sort_by(|a, b| {
        b.enabled
            .cmp(&a.enabled)
            .then_with(|| a.normalized_priority().cmp(&b.normalized_priority()))
    });

    // Pass 2: slot_id + role 匹配（仅健康会话）
    for slot in &ordered_slots {
        if matched.contains_key(&slot.slot_id) || skipped_slots.contains(&slot.slot_id) {
            continue;
        }
        let target_id = slot.slot_id.trim();
        if target_id.is_empty() {
            continue;
        }
        if let Some(found) = sorted_available.iter().find(|s| {
            s.healthy_for_reuse
                && !used_conversations.contains(&s.conversation_id)
                && s.role == slot.role
                && !s.slot_id.trim().is_empty()
                && s.slot_id.trim().eq_ignore_ascii_case(target_id)
        }) {
            used_conversations.insert(found.conversation_id.clone());
            matched.insert(slot.slot_id.clone(), found.clone());
        }
    }

    // Pass 3: alias (忽略大小写) + role 匹配（仅健康会话）
    for slot in &ordered_slots {
        if matched.contains_key(&slot.slot_id) || skipped_slots.contains(&slot.slot_id) {
            continue;
        }
        let slot_alias = slot.alias.trim().trim_start_matches('@');
        if slot_alias.is_empty() {
            continue;
        }
        if let Some(found) = sorted_available.iter().find(|s| {
            s.healthy_for_reuse
                && !used_conversations.contains(&s.conversation_id)
                && s.role == slot.role
                && !s.alias.trim().is_empty()
                && s.alias
                    .trim()
                    .trim_start_matches('@')
                    .eq_ignore_ascii_case(slot_alias)
        }) {
            used_conversations.insert(found.conversation_id.clone());
            matched.insert(slot.slot_id.clone(), found.clone());
        }
    }

    // Pass 4: 同 role 最近未分配健康会话回退匹配
    for slot in &ordered_slots {
        if matched.contains_key(&slot.slot_id) || skipped_slots.contains(&slot.slot_id) {
            continue;
        }
        if let Some(found) = sorted_available.iter().find(|s| {
            s.healthy_for_reuse
                && !used_conversations.contains(&s.conversation_id)
                && s.role == slot.role
        }) {
            used_conversations.insert(found.conversation_id.clone());
            matched.insert(slot.slot_id.clone(), found.clone());
        }
    }

    matched
}

pub(super) fn enrich_candidates_with_tail_health(
    candidates: &mut [WorkspaceRoleSession],
    brain_dir: Option<&Path>,
) {
    let Some(dir) = brain_dir else {
        return;
    };
    for item in candidates.iter_mut() {
        if let Some(health) = evaluate_session_health(Some(dir), &item.conversation_id) {
            item.context_tokens = health.context_tokens;
            item.healthy_for_reuse = health.healthy_for_reuse;
            item.unhealthy_reason = health.unhealthy_reason;
        }
    }
}

pub(super) fn collect_from_history_rooms(
    storage: &AppStorage,
    workspace: &str,
    target_key: &str,
    out: &mut Vec<WorkspaceRoleSession>,
) {
    let Ok(entries) = fs::read_dir(storage.rooms_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let Some(room_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if validate_room_id(&room_id).is_err() {
            continue;
        }
        let room_dir = entry.path();
        let snap = AppStorage::read_room_snapshot(&room_dir);
        let snap_ws_matches = snap
            .as_ref()
            .map(|sn| normalize_workspace_key(&sn.workspace) == target_key)
            .unwrap_or(false);

        let sessions_path = room_dir.join("sessions.json");
        let Ok(raw) = fs::read_to_string(&sessions_path) else {
            continue;
        };
        let Ok(raw_list) = serde_json::from_str::<Vec<WorkspaceRoleSession>>(
            raw.trim_start_matches('\u{FEFF}').trim(),
        ) else {
            continue;
        };

        let fallback_ts = snap
            .as_ref()
            .map(|sn| sn.updated_at.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| file_mtime_rfc3339(&sessions_path));

        for mut s in raw_list {
            if s.workspace.trim().is_empty() {
                if let Some(ref sn) = snap {
                    s.workspace = sn.workspace.clone();
                } else if snap_ws_matches {
                    s.workspace = workspace.trim().to_string();
                }
            }
            if s.updated_at.trim().is_empty() {
                s.updated_at = fallback_ts.clone();
            }
            if s.room_id.is_none() {
                s.room_id = Some(room_id.clone());
            }
            if s.last_goal.trim().is_empty() {
                if let Some(ref sn) = snap {
                    s.last_goal = sn.goal.clone();
                }
            }
            if let Some(clean) = sanitize_session(s, SESSION_SOURCE_CONDUCTOR, true) {
                if clean.workspace_key == target_key {
                    out.push(clean);
                }
            }
        }
    }
}

pub(super) fn collect_from_persisted_config(
    storage: &AppStorage,
    cfg: &PersistedConfig,
    workspace: &str,
    target_key: &str,
    brain_sessions: &[WorkspaceRoleSession],
    out: &mut Vec<WorkspaceRoleSession>,
) {
    if normalize_workspace_key(&cfg.last_workspace) != target_key {
        return;
    }
    let config_ts = file_mtime_rfc3339(&storage.config_path());

    for slot in &cfg.slots {
        let Some(cid) = slot
            .conversation_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };

        let already_indexed = out.iter().any(|existing| {
            existing.conversation_id == cid
                || (existing.role == slot.role
                    && existing.slot_id.eq_ignore_ascii_case(&slot.slot_id))
        });
        if already_indexed {
            continue;
        }

        let matched_brain = brain_sessions.iter().find(|b| b.conversation_id == cid);
        let ts = matched_brain
            .map(|b| b.updated_at.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| config_ts.clone());

        out.push(WorkspaceRoleSession {
            workspace: if cfg.last_workspace.trim().is_empty() {
                workspace.trim().to_string()
            } else {
                cfg.last_workspace.trim().to_string()
            },
            workspace_key: target_key.to_string(),
            slot_id: slot.slot_id.clone(),
            alias: slot.alias.clone(),
            display_name: slot.display_name.clone(),
            role: slot.role,
            model_id: slot.model_id.clone(),
            conversation_id: cid.to_string(),
            last_goal: String::new(),
            room_id: None,
            updated_at: ts,
            source: SESSION_SOURCE_CONDUCTOR.to_string(),
            context_tokens: matched_brain.map(|b| b.context_tokens).unwrap_or(0),
            healthy_for_reuse: matched_brain.map(|b| b.healthy_for_reuse).unwrap_or(true),
            unhealthy_reason: matched_brain.and_then(|b| b.unhealthy_reason.clone()),
        });
    }
}

pub(super) fn merge_and_dedup_discovered(
    candidates: Vec<WorkspaceRoleSession>,
) -> Vec<WorkspaceRoleSession> {
    let mut by_conv: Vec<WorkspaceRoleSession> = Vec::new();
    for item in candidates {
        if let Some(existing) = by_conv
            .iter_mut()
            .find(|e| e.conversation_id == item.conversation_id)
        {
            merge_same_conversation(existing, item);
        } else {
            by_conv.push(item);
        }
    }

    by_conv.sort_by(compare_recency_desc);

    let mut deduped: Vec<WorkspaceRoleSession> = Vec::new();
    for item in by_conv {
        // 仅在「同角色槽位 + 同健康状态」之间去重，防止更新但膨胀/中断的旧会话覆盖掉较早的健康轻量会话。
        if let Some(existing) = deduped
            .iter_mut()
            .find(|e| same_role_slot_identity(e, &item) && e.healthy_for_reuse == item.healthy_for_reuse)
        {
            if existing.last_goal.is_empty() && !item.last_goal.is_empty() {
                existing.last_goal = item.last_goal;
            }
            if existing.model_id.is_empty() && !item.model_id.is_empty() {
                existing.model_id = item.model_id;
            }
        } else {
            deduped.push(item);
        }
    }

    sort_sessions_by_role_and_recency(&mut deduped);
    deduped
}

fn merge_same_conversation(target: &mut WorkspaceRoleSession, incoming: WorkspaceRoleSession) {
    let incoming_newer = compare_recency_desc(&incoming, target).is_lt();
    if incoming_newer {
        target.updated_at = incoming.updated_at.clone();
    }

    let incoming_is_conductor = incoming.source == SESSION_SOURCE_CONDUCTOR;
    let target_is_conductor = target.source == SESSION_SOURCE_CONDUCTOR;
    let prefer_incoming = (incoming_is_conductor && !target_is_conductor)
        || (incoming_newer && incoming_is_conductor == target_is_conductor);

    if prefer_incoming {
        target.slot_id = incoming.slot_id;
        target.alias = incoming.alias;
        target.display_name = incoming.display_name;
        if incoming_is_conductor {
            target.source = SESSION_SOURCE_CONDUCTOR.to_string();
        }
    }

    if !incoming.model_id.is_empty() && (prefer_incoming || target.model_id.is_empty()) {
        target.model_id = incoming.model_id;
    }
    if (target.last_goal.is_empty() || incoming_newer) && !incoming.last_goal.is_empty() {
        target.last_goal = incoming.last_goal;
    }
    if (target.room_id.is_none() || incoming_newer) && incoming.room_id.is_some() {
        target.room_id = incoming.room_id;
    }
    if incoming.context_tokens > target.context_tokens {
        target.context_tokens = incoming.context_tokens;
    }
    if !incoming.healthy_for_reuse {
        target.healthy_for_reuse = false;
        if incoming.unhealthy_reason.is_some() {
            target.unhealthy_reason = incoming.unhealthy_reason;
        }
    }
}

fn same_role_slot_identity(a: &WorkspaceRoleSession, b: &WorkspaceRoleSession) -> bool {
    if a.role != b.role {
        return false;
    }
    let same_slot_id = !a.slot_id.is_empty()
        && !b.slot_id.is_empty()
        && a.slot_id.eq_ignore_ascii_case(&b.slot_id);
    let same_alias = !a.alias.is_empty()
        && !b.alias.is_empty()
        && a.alias.eq_ignore_ascii_case(&b.alias);
    same_slot_id || same_alias
}

pub(super) fn sort_sessions_by_recency(list: &mut [WorkspaceRoleSession]) {
    list.sort_by(compare_recency_desc);
}

pub(super) fn sort_sessions_by_role_and_recency(list: &mut [WorkspaceRoleSession]) {
    list.sort_by(|a, b| {
        role_order(a.role)
            .cmp(&role_order(b.role))
            .then_with(|| compare_recency_desc(a, b))
            .then_with(|| a.slot_id.cmp(&b.slot_id))
    });
}

fn compare_recency_desc(
    a: &WorkspaceRoleSession,
    b: &WorkspaceRoleSession,
) -> std::cmp::Ordering {
    parse_rfc3339_instant(&b.updated_at)
        .cmp(&parse_rfc3339_instant(&a.updated_at))
        .then_with(|| b.updated_at.cmp(&a.updated_at))
        .then_with(|| {
            let a_cond = a.source == SESSION_SOURCE_CONDUCTOR;
            let b_cond = b.source == SESSION_SOURCE_CONDUCTOR;
            b_cond.cmp(&a_cond)
        })
}

fn role_order(role: RoleKind) -> u8 {
    match role {
        RoleKind::Architect => 0,
        RoleKind::Coder => 1,
        RoleKind::Reviewer => 2,
    }
}

pub(super) fn role_default_slug(role: RoleKind) -> &'static str {
    match role {
        RoleKind::Architect => "architect",
        RoleKind::Coder => "coder",
        RoleKind::Reviewer => "reviewer",
    }
}

fn parse_rfc3339_instant(ts: &str) -> Option<(i64, u32)> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| (dt.timestamp(), dt.timestamp_subsec_nanos()))
}

fn file_mtime_rfc3339(path: &Path) -> String {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mtime| DateTime::<Local>::from(mtime).to_rfc3339())
        .unwrap_or_default()
}
