use crate::orchestrator::types::{TaskItem, TaskStatus};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
enum NormalizedScope {
    WholeRepo,
    Prefix(Vec<String>),
}

fn normalize_path_str(raw: &str) -> String {
    let mut s = raw.trim().replace('\\', "/");
    if let Some(stripped) = s.strip_prefix("//?/") {
        s = stripped.to_string();
    }
    let lowered = s.to_ascii_lowercase();
    if lowered == "/" {
        lowered
    } else {
        lowered.trim_end_matches('/').to_string()
    }
}

fn has_drive_prefix(path_norm: &str) -> bool {
    let bytes = path_norm.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn has_wildcard_chars(segment: &str) -> bool {
    segment.contains('*') || segment.contains('?') || segment.contains('[')
}

fn normalize_scope(raw: &str, workspace_norm: Option<&str>) -> NormalizedScope {
    let norm = normalize_path_str(raw);
    if norm.is_empty() || norm == "." || norm == "*" {
        return NormalizedScope::WholeRepo;
    }

    let rel: &str = if let Some(ws) = workspace_norm.filter(|w| !w.is_empty()) {
        if norm == ws {
            return NormalizedScope::WholeRepo;
        } else if let Some(rest) = norm.strip_prefix(ws).filter(|r| r.starts_with('/')) {
            &rest[1..]
        } else if has_drive_prefix(&norm)
            || norm.starts_with("//")
            || (ws.starts_with('/') && norm.starts_with('/'))
        {
            return NormalizedScope::WholeRepo;
        } else {
            norm.trim_start_matches('/')
        }
    } else if has_drive_prefix(&norm) || norm.starts_with("//") {
        return NormalizedScope::WholeRepo;
    } else {
        norm.trim_start_matches('/')
    };

    let mut resolved: Vec<String> = Vec::new();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        // 中间通配符截断锁：遇 `*` / `?` / `[` 立即截断至其父目录前缀；若首段即通配则退化为全仓锁
        if has_wildcard_chars(seg) {
            break;
        }
        if seg == ".." {
            if resolved.pop().is_none() {
                return NormalizedScope::WholeRepo;
            }
            continue;
        }
        resolved.push(seg.to_string());
    }

    if resolved.is_empty() {
        NormalizedScope::WholeRepo
    } else {
        NormalizedScope::Prefix(resolved)
    }
}

fn scopes_overlap(a: &NormalizedScope, b: &NormalizedScope) -> bool {
    match (a, b) {
        (NormalizedScope::WholeRepo, _) | (_, NormalizedScope::WholeRepo) => true,
        (NormalizedScope::Prefix(pa), NormalizedScope::Prefix(pb)) => {
            let min_len = pa.len().min(pb.len());
            pa[..min_len] == pb[..min_len]
        }
    }
}

/// 在给定可选工作区根目录下，判断两组 `owned_paths` 是否存在交集、父子目录包含、
/// 通配符截断重叠或全仓锁冲突。
pub fn paths_conflict_in_workspace(
    a: &[String],
    b: &[String],
    workspace_dir: Option<&Path>,
) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    let ws_norm = workspace_dir.map(|p| normalize_path_str(&p.to_string_lossy()));
    let ws_ref = ws_norm.as_deref();

    let scopes_a: Vec<NormalizedScope> = a.iter().map(|p| normalize_scope(p, ws_ref)).collect();
    let scopes_b: Vec<NormalizedScope> = b.iter().map(|p| normalize_scope(p, ws_ref)).collect();

    for sa in &scopes_a {
        for sb in &scopes_b {
            if scopes_overlap(sa, sb) {
                return true;
            }
        }
    }
    false
}

/// Returns `true` if two `owned_paths` lists overlap, share a parent/child directory
/// containment relationship, or either list represents a whole-repo lock (empty or `*`).
pub fn paths_conflict(a: &[String], b: &[String]) -> bool {
    paths_conflict_in_workspace(a, b, None)
}

/// Checks whether a candidate task conflicts with any currently active task
/// (`Coding` or `InReview` or `PendingReview`) in the workspace.
pub fn has_active_path_conflict_in_workspace(
    candidate: &TaskItem,
    all_tasks: &[TaskItem],
    workspace_dir: Option<&Path>,
) -> Option<String> {
    for other in all_tasks {
        if other.task_id == candidate.task_id {
            continue;
        }
        let holds_lock = matches!(
            other.status,
            TaskStatus::Coding | TaskStatus::InReview | TaskStatus::PendingReview
        );
        if holds_lock
            && paths_conflict_in_workspace(&candidate.owned_paths, &other.owned_paths, workspace_dir)
        {
            return Some(other.task_id.clone());
        }
    }
    None
}

/// Checks whether a candidate task conflicts with any currently active task
/// (`Coding` or `InReview` or `PendingReview`) without workspace prefix stripping.
pub fn has_active_path_conflict(candidate: &TaskItem, all_tasks: &[TaskItem]) -> Option<String> {
    has_active_path_conflict_in_workspace(candidate, all_tasks, None)
}

/// Checks whether all `depends_on` predecessors of `task` exist and have been `Approved`.
pub fn dependencies_satisfied(task: &TaskItem, all_tasks: &[TaskItem]) -> bool {
    task.depends_on.iter().all(|dep_id| {
        all_tasks
            .iter()
            .find(|t| t.task_id == *dep_id)
            .map(|t| t.status == TaskStatus::Approved)
            .unwrap_or(false)
    })
}

/// Detects whether a workspace contains Cargo/Node manifests where concurrent full test runs
/// could contend on build locks.
pub fn workspace_has_shared_build_lock(workspace_dir: &Path) -> bool {
    workspace_dir.join("Cargo.toml").is_file() || workspace_dir.join("package.json").is_file()
}
