use crate::orchestrator::types::{TaskItem, TaskStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
enum NormalizedScope {
    WholeRepo,
    Prefix(Vec<String>),
}

fn normalize_scope(raw: &str) -> NormalizedScope {
    let trimmed = raw.trim().replace('\\', "/");
    let stripped = trimmed
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches("/**")
        .trim_end_matches("/*")
        .trim_end_matches('/');

    if stripped.is_empty() || stripped == "*" || stripped == "." {
        return NormalizedScope::WholeRepo;
    }

    let segments: Vec<String> = stripped
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(|s| s.to_ascii_lowercase())
        .collect();

    if segments.is_empty() {
        NormalizedScope::WholeRepo
    } else {
        NormalizedScope::Prefix(segments)
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

/// Returns `true` if two `owned_paths` lists overlap, share a parent/child directory
/// containment relationship, or either list represents a whole-repo lock (empty or `*`).
pub fn paths_conflict(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    let scopes_a: Vec<NormalizedScope> = a.iter().map(|p| normalize_scope(p)).collect();
    let scopes_b: Vec<NormalizedScope> = b.iter().map(|p| normalize_scope(p)).collect();

    for sa in &scopes_a {
        for sb in &scopes_b {
            if scopes_overlap(sa, sb) {
                return true;
            }
        }
    }
    false
}

/// Checks whether a candidate task conflicts with any currently active task
/// (`Coding` or `InReview` or `PendingReview`) in the workspace.
pub fn has_active_path_conflict(candidate: &TaskItem, all_tasks: &[TaskItem]) -> Option<String> {
    for other in all_tasks {
        if other.task_id == candidate.task_id {
            continue;
        }
        let holds_lock = matches!(
            other.status,
            TaskStatus::Coding | TaskStatus::InReview | TaskStatus::PendingReview
        );
        if holds_lock && paths_conflict(&candidate.owned_paths, &other.owned_paths) {
            return Some(other.task_id.clone());
        }
    }
    None
}

/// Checks whether all `depends_on` predecessors of `task` have been `Approved`.
pub fn dependencies_satisfied(task: &TaskItem, all_tasks: &[TaskItem]) -> bool {
    task.depends_on.iter().all(|dep_id| {
        all_tasks
            .iter()
            .find(|t| t.task_id == *dep_id)
            .map(|t| t.status == TaskStatus::Approved)
            .unwrap_or(true)
    })
}

/// Detects whether a workspace contains Cargo/Node manifests where concurrent full test runs
/// could contend on build locks.
pub fn workspace_has_shared_build_lock(workspace_dir: &std::path::Path) -> bool {
    workspace_dir.join("Cargo.toml").is_file() || workspace_dir.join("package.json").is_file()
}
