use crate::orchestrator::types::{
    GroupChatMessage, MessageBadge, ReviewIssue, TaskItem, TaskStatus,
};
use crate::storage::store::{atomic_write_text, AppStorage};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

const MAX_ROOM_ID_LEN: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryRoomSummary {
    pub room_id: String,
    pub workspace: String,
    pub goal: String,
    pub phase: String,
    pub task_count: usize,
    pub approved_task_count: usize,
    pub message_count: usize,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRoomDetail {
    pub room_id: String,
    pub workspace: String,
    pub goal: String,
    pub phase: String,
    pub updated_at: String,
    pub tasks: Vec<TaskItem>,
    pub messages: Vec<GroupChatMessage>,
}

/// 严格校验 `room_id`，禁止路径穿越、路径分隔符、控制字符及隐藏/特殊目录名。
pub fn validate_room_id(room_id: &str) -> Result<(), String> {
    if room_id.is_empty() || room_id != room_id.trim() {
        return Err("无效的历史会话 ID：不能为空或包含首尾空白".to_string());
    }
    if room_id.len() > MAX_ROOM_ID_LEN {
        return Err(format!("无效的历史会话 ID：长度不能超过 {MAX_ROOM_ID_LEN}"));
    }
    if room_id.starts_with('.') || room_id.ends_with('.') || room_id.contains("..") {
        return Err("无效的历史会话 ID：禁止包含相对路径片段".to_string());
    }
    if !room_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("无效的历史会话 ID：仅允许字母、数字、连字符、下划线与点号".to_string());
    }
    let mut components = Path::new(room_id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err("无效的历史会话 ID：必须为单层目录名".to_string()),
    }
}

/// 读取 `rooms/` 目录下全部有效历史会话摘要，按 `updated_at` 倒序排列。
/// 兼容仅有 `board.jsonl` 的旧记录，自动忽略既无快照又无消息的空目录。
pub fn list_history_rooms(storage: &AppStorage) -> Result<Vec<HistoryRoomSummary>, String> {
    let rooms_root = storage.rooms_dir();
    if !rooms_root.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(&rooms_root)
        .map_err(|e| format!("读取历史目录失败 {}: {e}", rooms_root.display()))?;
    let mut rooms = Vec::new();

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
        if let Some(detail) = read_room_detail_from_dir(&entry.path(), &room_id) {
            let approved_task_count = detail
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Approved)
                .count();
            rooms.push(HistoryRoomSummary {
                room_id: detail.room_id,
                workspace: detail.workspace,
                goal: detail.goal,
                phase: detail.phase,
                task_count: detail.tasks.len(),
                approved_task_count,
                message_count: detail.messages.len(),
                updated_at: detail.updated_at,
            });
        }
    }

    rooms.sort_by(|a, b| {
        parse_rfc3339_instant(&b.updated_at)
            .cmp(&parse_rfc3339_instant(&a.updated_at))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| b.room_id.cmp(&a.room_id))
    });
    Ok(rooms)
}

/// 加载指定 `room_id` 的历史会话完整详情（含子任务列表与群聊消息流）。
pub fn load_history_room(storage: &AppStorage, room_id: &str) -> Result<HistoryRoomDetail, String> {
    validate_room_id(room_id)?;
    let room_dir = resolve_safe_room_dir(storage, room_id)?;
    read_room_detail_from_dir(&room_dir, room_id)
        .ok_or_else(|| format!("历史记录 {room_id} 不包含有效的状态快照或群聊对话"))
}

/// 安全删除指定 `room_id` 的历史记录目录（严格校验路径穿越与目录归属）。
pub fn delete_history_room(storage: &AppStorage, room_id: &str) -> Result<(), String> {
    validate_room_id(room_id)?;
    let room_dir = resolve_safe_room_dir(storage, room_id)?;
    fs::remove_dir_all(&room_dir)
        .map_err(|e| format!("删除历史记录目录失败 {}: {e}", room_dir.display()))
}

/// 为指定历史记录生成安全默认的 Markdown 导出文件名。
pub fn suggested_markdown_filename(detail: &HistoryRoomDetail) -> String {
    let safe: String = detail
        .room_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '-' })
        .collect();
    let suffix = if safe.is_empty() { "report" } else { safe.as_str() };
    format!("agy-history-{suffix}.md")
}

/// 将单条历史项目记录渲染为结构清晰的 Markdown 报告（纯函数）。
pub fn render_history_markdown(detail: &HistoryRoomDetail) -> String {
    let approved = detail
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Approved)
        .count();
    let workspace = non_empty_or(&detail.workspace, "未记录");
    let goal = non_empty_or(&detail.goal, "未记录项目目标");

    let mut md = String::with_capacity(1024);
    let _ = writeln!(md, "# AgyConductor 项目协同历史报告\n");
    let _ = writeln!(md, "## 基本信息\n");
    let _ = writeln!(md, "- **会话 ID**：`{}`", detail.room_id);
    let _ = writeln!(md, "- **工作目录**：`{workspace}`");
    let _ = writeln!(md, "- **当前阶段**：{}", detail.phase);
    let _ = writeln!(md, "- **更新时间**：{}", detail.updated_at);
    let _ = writeln!(md, "- **子任务进度**：{approved} / {} 已验收通过", detail.tasks.len());
    let _ = writeln!(md, "- **群聊消息数**：{} 条\n", detail.messages.len());

    let _ = writeln!(md, "## 项目长期目标\n\n{goal}\n");
    render_tasks_section(&mut md, &detail.tasks);
    render_messages_section(&mut md, &detail.messages);
    md
}

/// 渲染并将历史项目报告原子写入目标 `.md` 文件。
pub fn export_history_markdown(detail: &HistoryRoomDetail, target_path: &Path) -> Result<(), String> {
    atomic_write_text(target_path, &render_history_markdown(detail))
}

fn render_tasks_section(md: &mut String, tasks: &[TaskItem]) {
    let _ = writeln!(md, "## 子任务状态\n");
    if tasks.is_empty() {
        let _ = writeln!(md, "> 本次记录暂无子任务快照。\n");
        return;
    }

    let _ = writeln!(md, "| 任务 ID | 标题 | 状态 | 负责成员 | 返工次数 | 归属路径 |");
    let _ = writeln!(md, "| --- | --- | --- | --- | --- | --- |");
    for t in tasks {
        let assignee = t.assigned_coder_label.as_deref().unwrap_or("-");
        let paths = if t.owned_paths.is_empty() { "-".to_string() } else { t.owned_paths.join(", ") };
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} |",
            escape_md_cell(&t.task_id),
            escape_md_cell(&t.title),
            task_status_label_zh(t.status),
            escape_md_cell(assignee),
            t.rework_count,
            escape_md_cell(&paths)
        );
    }
    let _ = writeln!(md);

    for t in tasks {
        let has_extra = t.last_coder_summary.is_some()
            || t.last_self_check.is_some()
            || t.last_review_note.is_some()
            || t.blocker_reason.is_some()
            || !t.last_modified_files.is_empty()
            || !t.last_review_issues.is_empty();
        if !has_extra {
            continue;
        }
        let _ = writeln!(md, "### {} — {}\n", t.task_id, t.title);
        push_opt_bullet(md, "最新实现摘要", t.last_coder_summary.as_deref());
        push_opt_bullet(md, "自测结果", t.last_self_check.as_deref());
        if !t.last_modified_files.is_empty() {
            let _ = writeln!(md, "- **变更文件**：{}", format_code_list(&t.last_modified_files));
        }
        push_opt_bullet(md, "审查结论", t.last_review_note.as_deref());
        push_opt_bullet(md, "阻塞原因", t.blocker_reason.as_deref());
        for issue in &t.last_review_issues {
            let _ = writeln!(md, "  - {}", format_review_issue(issue));
        }
        let _ = writeln!(md);
    }
}

fn render_messages_section(md: &mut String, messages: &[GroupChatMessage]) {
    let _ = writeln!(md, "## 群聊对话记录\n");
    if messages.is_empty() {
        let _ = writeln!(md, "> 本次记录暂无群聊消息。");
        return;
    }
    for msg in messages {
        let mut tags = Vec::new();
        if let Some(role) = msg.sender_role {
            tags.push(role.label_zh().to_string());
        }
        if let Some(prio) = msg.priority {
            tags.push(format!("P{prio}"));
        }
        if let Some(ref tid) = msg.task_id {
            if !tid.trim().is_empty() {
                tags.push(tid.clone());
            }
        }
        if let Some(badge) = badge_label_zh(msg.badge) {
            tags.push(badge.to_string());
        }
        let suffix = if tags.is_empty() { String::new() } else { format!(" ({})", tags.join(" · ")) };
        let _ = writeln!(md, "### [{}] {}{}\n", msg.timestamp, msg.sender_display, suffix);
        let _ = writeln!(md, "{}\n", msg.content.trim());

        if !msg.issues.is_empty() {
            let _ = writeln!(md, "**审查问题清单：**\n");
            for issue in &msg.issues {
                let _ = writeln!(md, "- {}", format_review_issue(issue));
            }
            let _ = writeln!(md);
        }

        let mut meta = Vec::new();
        if !msg.modified_files.is_empty() {
            meta.push(format!("变更文件：{}", format_code_list(&msg.modified_files)));
        }
        if let Some(sec) = msg.duration_seconds.filter(|s| *s > 0.0) {
            meta.push(format!("耗时 {}s", sec.round() as u64));
        }
        if let Some(steps) = msg.tool_steps.filter(|s| *s > 0) {
            meta.push(format!("{steps} 步工具操作"));
        }
        if !meta.is_empty() {
            let _ = writeln!(md, "> {}\n", meta.join(" · "));
        }
    }
}

fn resolve_safe_room_dir(storage: &AppStorage, room_id: &str) -> Result<PathBuf, String> {
    let rooms_root = storage.rooms_dir();
    let room_dir = rooms_root.join(room_id);
    let meta = fs::symlink_metadata(&room_dir)
        .map_err(|_| format!("未找到历史项目记录: {room_id}"))?;
    if meta.file_type().is_symlink() {
        return Err(format!("拒绝访问符号链接历史目录: {room_id}"));
    }
    if !meta.is_dir() {
        return Err(format!("历史记录目标不是目录: {room_id}"));
    }
    let canon_root = fs::canonicalize(&rooms_root)
        .map_err(|e| format!("解析历史根目录失败: {e}"))?;
    let canon_dir = fs::canonicalize(&room_dir)
        .map_err(|e| format!("解析历史记录目录失败: {e}"))?;
    if !canon_dir.starts_with(&canon_root) || canon_dir == canon_root {
        return Err("检测到非法历史目录路径，已拒绝操作".to_string());
    }
    Ok(room_dir)
}

fn read_room_detail_from_dir(room_dir: &Path, room_id: &str) -> Option<HistoryRoomDetail> {
    let snapshot = AppStorage::read_room_snapshot(room_dir);
    let messages = AppStorage::read_board_messages(room_dir).unwrap_or_default();
    if snapshot.is_none() && messages.is_empty() {
        return None;
    }

    if let Some(snap) = snapshot {
        Some(HistoryRoomDetail {
            room_id: room_id.to_string(),
            workspace: snap.workspace,
            goal: snap.goal,
            phase: non_empty_or(&snap.phase, "未知阶段").to_string(),
            updated_at: if snap.updated_at.trim().is_empty() {
                fallback_dir_updated_at(room_dir)
            } else {
                snap.updated_at
            },
            tasks: snap.tasks,
            messages,
        })
    } else {
        Some(HistoryRoomDetail {
            room_id: room_id.to_string(),
            workspace: String::new(),
            goal: infer_goal_from_messages(&messages),
            phase: "仅对话记录".to_string(),
            updated_at: fallback_dir_updated_at(room_dir),
            tasks: Vec::new(),
            messages,
        })
    }
}

fn infer_goal_from_messages(messages: &[GroupChatMessage]) -> String {
    for msg in messages {
        if let Some(rest) = msg.content.trim().strip_prefix("已启动长期目标自动协同：") {
            let core = rest.split("（目录：").next().unwrap_or(rest).trim();
            if !core.is_empty() {
                return truncate_chars(core, 120);
            }
        }
    }
    messages
        .iter()
        .filter(|m| m.is_user)
        .chain(messages.iter())
        .filter_map(|m| m.content.lines().next().map(str::trim).filter(|s| !s.is_empty()))
        .next()
        .map(|s| truncate_chars(s, 120))
        .unwrap_or_else(|| "历史会话（无状态快照）".to_string())
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max_chars).collect::<String>())
    }
}

fn fallback_dir_updated_at(room_dir: &Path) -> String {
    for path in [room_dir.join("state.json"), room_dir.join("board.jsonl"), room_dir.to_path_buf()] {
        if let Ok(mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
            return DateTime::<Local>::from(mtime).to_rfc3339();
        }
    }
    String::new()
}

fn parse_rfc3339_instant(ts: &str) -> Option<(i64, u32)> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| (dt.timestamp(), dt.timestamp_subsec_nanos()))
}

fn non_empty_or<'a>(raw: &'a str, fallback: &'a str) -> &'a str {
    let trimmed = raw.trim();
    if trimmed.is_empty() { fallback } else { trimmed }
}

fn push_opt_bullet(md: &mut String, label: &str, val: Option<&str>) {
    if let Some(text) = val.map(str::trim).filter(|s| !s.is_empty()) {
        let _ = writeln!(md, "- **{label}**：{text}");
    }
}

fn format_code_list(items: &[String]) -> String {
    items.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ")
}

fn format_review_issue(issue: &ReviewIssue) -> String {
    let loc = match issue.line_hint.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(line) => format!("{}:{line}", issue.file_path),
        None => issue.file_path.clone(),
    };
    format!("**[{}]** `{loc}` — {}", issue.severity, issue.detail)
}

fn escape_md_cell(raw: &str) -> String {
    raw.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn task_status_label_zh(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "待分配",
        TaskStatus::WaitingPathLock => "等待路径锁",
        TaskStatus::Coding => "编码中",
        TaskStatus::PendingReview => "待审查",
        TaskStatus::InReview => "审查中",
        TaskStatus::ReworkNeeded => "待返工",
        TaskStatus::Approved => "已通过",
        TaskStatus::BlockedByArch => "架构阻塞",
        TaskStatus::NeedsUserDecision => "待人工决策",
    }
}

fn badge_label_zh(badge: MessageBadge) -> Option<&'static str> {
    match badge {
        MessageBadge::None | MessageBadge::Info => None,
        MessageBadge::Plan => Some("规划派单"),
        MessageBadge::Submit => Some("提请审查"),
        MessageBadge::Approved => Some("验收通过"),
        MessageBadge::Rework => Some("打回修改"),
        MessageBadge::Blocked => Some("架构阻塞"),
        MessageBadge::FinalAccept => Some("全局终验通过"),
        MessageBadge::User => Some("人工指导"),
        MessageBadge::Warning => Some("告警"),
    }
}
