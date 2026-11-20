use crate::orchestrator::schemas::PlannedTaskSpec;
use crate::orchestrator::types::{TaskItem, TaskStatus};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeTasksOutcome {
    pub added: usize,
    /// 处于活跃态（Coding / PendingReview / InReview）且核心契约发生变更、
    /// 需要取消旧在途轮次并重置为 Pending 的任务 ID 列表。
    pub invalidated_task_ids: Vec<String>,
}

fn task_contract_changed(prev: &TaskItem, spec: &PlannedTaskSpec) -> bool {
    prev.description != spec.description
        || prev.owned_paths != spec.owned_paths
        || prev.depends_on != spec.depends_on
        || prev.acceptance_criteria != spec.acceptance_criteria
}

/// 安全合并总架构师规划的子任务清单：
/// - `Approved` 任务保持只读不动；
/// - 活跃态（`Coding` / `PendingReview` / `InReview`）任务若契约未变，仅更新展示性标题与建议别名，绝不打断在途执行；
/// - 活跃态任务若契约确已变更，记录到 `invalidated_task_ids` 并安全重置为 `Pending`，由调用方先取消旧轮次再派新单。
pub fn merge_planned_tasks_safely(
    existing: &mut Vec<TaskItem>,
    planned: Vec<PlannedTaskSpec>,
) -> MergeTasksOutcome {
    let mut outcome = MergeTasksOutcome::default();

    for spec in planned {
        if let Some(prev) = existing.iter_mut().find(|t| t.task_id == spec.task_id) {
            if prev.status == TaskStatus::Approved {
                continue;
            }

            let is_active = matches!(
                prev.status,
                TaskStatus::Coding | TaskStatus::PendingReview | TaskStatus::InReview
            );

            if is_active {
                if !task_contract_changed(prev, &spec) {
                    prev.title = spec.title;
                    prev.preferred_coder_alias = spec.preferred_coder_alias;
                    continue;
                }
                outcome.invalidated_task_ids.push(prev.task_id.clone());
                prev.title = spec.title;
                prev.description = spec.description;
                prev.owned_paths = spec.owned_paths;
                prev.depends_on = spec.depends_on;
                prev.preferred_coder_alias = spec.preferred_coder_alias;
                prev.acceptance_criteria = spec.acceptance_criteria;
                prev.status = TaskStatus::Pending;
                prev.assigned_coder_slot_id = None;
                prev.assigned_coder_label = None;
                prev.rework_count = 0;
                prev.blocker_reason = None;
                prev.last_review_issues.clear();
                prev.last_review_note = None;
                continue;
            }

            prev.title = spec.title;
            prev.description = spec.description;
            prev.owned_paths = spec.owned_paths;
            prev.depends_on = spec.depends_on;
            prev.preferred_coder_alias = spec.preferred_coder_alias;
            prev.acceptance_criteria = spec.acceptance_criteria;
            if matches!(
                prev.status,
                TaskStatus::BlockedByArch | TaskStatus::NeedsUserDecision
            ) {
                prev.status = TaskStatus::Pending;
                prev.blocker_reason = None;
            } else if prev.status == TaskStatus::ReworkNeeded && prev.rework_count > 0 {
                prev.status = TaskStatus::Pending;
                prev.rework_count = 0;
                prev.blocker_reason = None;
            }
        } else {
            existing.push(TaskItem {
                task_id: spec.task_id,
                title: spec.title,
                description: spec.description,
                owned_paths: spec.owned_paths,
                depends_on: spec.depends_on,
                preferred_coder_alias: spec.preferred_coder_alias,
                acceptance_criteria: spec.acceptance_criteria,
                status: TaskStatus::Pending,
                assigned_coder_slot_id: None,
                assigned_coder_label: None,
                last_coder_slot_id: None,
                rework_count: 0,
                arch_arbitration_count: 0,
                coder_failure_streak: 0,
                last_coder_summary: None,
                last_modified_files: Vec::new(),
                last_self_check: None,
                last_review_issues: Vec::new(),
                last_review_note: None,
                blocker_reason: None,
            });
            outcome.added += 1;
        }
    }

    outcome
}

/// 校验任务依赖图是否为合法 DAG：
/// 1. 禁止自依赖（`dep == task_id`）；
/// 2. 禁止引用不存在于当前任务清单中的悬空 `depends_on`；
/// 3. 使用 Kahn 拓扑排序检测有向环路并返回含涉环节点列表的中文诊断。
pub fn validate_task_dag(tasks: &[TaskItem]) -> Result<(), String> {
    let mut id_to_idx: HashMap<&str, usize> = HashMap::with_capacity(tasks.len());
    for (idx, task) in tasks.iter().enumerate() {
        if id_to_idx.insert(task.task_id.as_str(), idx).is_some() {
            return Err(format!(
                "任务依赖图非法：存在重复的子任务 ID「{}」。",
                task.task_id
            ));
        }
    }

    let n = tasks.len();
    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (idx, task) in tasks.iter().enumerate() {
        let mut seen_deps: HashSet<&str> = HashSet::new();
        for dep in &task.depends_on {
            if dep == &task.task_id {
                return Err(format!(
                    "任务依赖图非法：子任务 {} 存在自依赖（depends_on 引用了自身）。",
                    task.task_id
                ));
            }
            let Some(&dep_idx) = id_to_idx.get(dep.as_str()) else {
                return Err(format!(
                    "任务依赖图非法：子任务 {} 依赖了不存在的前置任务 {}。",
                    task.task_id, dep
                ));
            };
            if seen_deps.insert(dep.as_str()) {
                adj[dep_idx].push(idx);
                in_degree[idx] += 1;
            }
        }
    }

    let mut queue: VecDeque<usize> = VecDeque::new();
    for (idx, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(idx);
        }
    }

    let mut visited = 0usize;
    while let Some(u) = queue.pop_front() {
        visited += 1;
        for &v in &adj[u] {
            in_degree[v] -= 1;
            if in_degree[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    if visited < n {
        let cycle_nodes: Vec<&str> = tasks
            .iter()
            .enumerate()
            .filter(|(idx, _)| in_degree[*idx] > 0)
            .map(|(_, t)| t.task_id.as_str())
            .collect();
        return Err(format!(
            "任务依赖图存在循环依赖，涉及子任务：{}，请打破环路并确保依赖关系为有向无环图（DAG）。",
            cycle_nodes.join("、")
        ));
    }

    Ok(())
}
