use agy_conductor::billing::SlotUsageStats;
use agy_conductor::orchestrator::priority_pool::{
    select_slot_for_new_task, select_slot_for_rework, SlotSelection,
};
use agy_conductor::orchestrator::types::{
    AgentSlot, RoleKind, SameRoleDispatchMode, SlotRunState, TaskItem, TaskStatus,
};
use agy_conductor::orchestrator::workspace_guard::{
    dependencies_satisfied, has_active_path_conflict, paths_conflict,
};

fn make_coder(slot_id: &str, alias: &str, priority: u8, state: SlotRunState) -> AgentSlot {
    AgentSlot {
        slot_id: slot_id.to_string(),
        alias: alias.to_string(),
        display_name: format!("Coder {alias}"),
        model_id: format!("[Gateway] {alias}"),
        role: RoleKind::Coder,
        priority,
        enabled: true,
        state,
        conversation_id: Some(format!("conv-{slot_id}")),
        session_reused: false,
        current_activity: None,
        consecutive_failures: 0,
        usage: SlotUsageStats::default(),
    }
}

fn make_task(task_id: &str, owned_paths: &[&str], depends_on: &[&str], status: TaskStatus) -> TaskItem {
    TaskItem {
        task_id: task_id.to_string(),
        title: format!("Task {task_id}"),
        description: "desc".to_string(),
        owned_paths: owned_paths.iter().map(|s| s.to_string()).collect(),
        depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
        preferred_coder_alias: None,
        acceptance_criteria: vec!["ok".to_string()],
        status,
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
    }
}

#[test]
fn selects_p1_neon_before_p2_hydrogen_and_overflows_when_p1_busy() {
    let mut slots = vec![
        make_coder("coder-hydrogen", "hydrogen", 2, SlotRunState::Idle),
        make_coder("coder-neon", "neon", 1, SlotRunState::Idle),
    ];

    // Single task: strictly picks P1 Neon regardless of slice ordering.
    let first = select_slot_for_new_task(
        RoleKind::Coder,
        None,
        &slots,
        SameRoleDispatchMode::PriorityWithOverflow,
    );
    assert_eq!(
        first,
        SlotSelection::Selected {
            slot_id: "coder-neon".to_string(),
            is_handoff: false
        }
    );

    // P1 Neon starts working -> second parallel task overflows to P2 Hydrogen.
    slots[1].state = SlotRunState::Working;
    let second = select_slot_for_new_task(
        RoleKind::Coder,
        None,
        &slots,
        SameRoleDispatchMode::PriorityWithOverflow,
    );
    assert_eq!(
        second,
        SlotSelection::Selected {
            slot_id: "coder-hydrogen".to_string(),
            is_handoff: false
        }
    );

    // Once P1 Neon finishes and returns to Idle, next task goes back to P1 Neon.
    slots[1].state = SlotRunState::Idle;
    let third = select_slot_for_new_task(
        RoleKind::Coder,
        None,
        &slots,
        SameRoleDispatchMode::PriorityWithOverflow,
    );
    assert_eq!(
        third,
        SlotSelection::Selected {
            slot_id: "coder-neon".to_string(),
            is_handoff: false
        }
    );
}

#[test]
fn strict_primary_failover_waits_for_busy_p1_and_fails_over_on_quota_cooling() {
    let mut slots = vec![
        make_coder("coder-neon", "neon", 1, SlotRunState::Working),
        make_coder("coder-hydrogen", "hydrogen", 2, SlotRunState::Idle),
    ];

    let waiting = select_slot_for_new_task(
        RoleKind::Coder,
        None,
        &slots,
        SameRoleDispatchMode::StrictPrimaryFailover,
    );
    assert_eq!(
        waiting,
        SlotSelection::WaitForBusyPrimary {
            primary_slot_id: "coder-neon".to_string()
        }
    );

    // P1 hits 429 QuotaCooling -> P2 Hydrogen seamlessly takes over.
    slots[0].state = SlotRunState::QuotaCooling;
    let failover = select_slot_for_new_task(
        RoleKind::Coder,
        None,
        &slots,
        SameRoleDispatchMode::StrictPrimaryFailover,
    );
    assert_eq!(
        failover,
        SlotSelection::Selected {
            slot_id: "coder-hydrogen".to_string(),
            is_handoff: false
        }
    );
}

#[test]
fn sticky_rework_prefers_original_coder_and_hands_off_on_quota_or_failure_streak() {
    let mut slots = vec![
        make_coder("coder-neon", "neon", 1, SlotRunState::Idle),
        make_coder("coder-hydrogen", "hydrogen", 2, SlotRunState::Idle),
    ];

    // Original author Hydrogen is Idle -> sticky rework routes back to Hydrogen (not P1 Neon).
    let sticky = select_slot_for_rework(
        Some("coder-hydrogen"),
        0,
        &slots,
        SameRoleDispatchMode::PriorityWithOverflow,
    );
    assert_eq!(
        sticky,
        SlotSelection::Selected {
            slot_id: "coder-hydrogen".to_string(),
            is_handoff: false
        }
    );

    // Original author Neon is currently busy -> wait for Neon to finish its turn first.
    slots[0].state = SlotRunState::Working;
    let wait_orig = select_slot_for_rework(
        Some("coder-neon"),
        0,
        &slots,
        SameRoleDispatchMode::PriorityWithOverflow,
    );
    assert_eq!(
        wait_orig,
        SlotSelection::WaitForOriginalCoder {
            original_slot_id: "coder-neon".to_string()
        }
    );

    // Original author Neon hits QuotaCooling -> cross-model handoff to Hydrogen.
    slots[0].state = SlotRunState::QuotaCooling;
    let quota_handoff = select_slot_for_rework(
        Some("coder-neon"),
        0,
        &slots,
        SameRoleDispatchMode::StrictPrimaryFailover,
    );
    assert_eq!(
        quota_handoff,
        SlotSelection::Selected {
            slot_id: "coder-hydrogen".to_string(),
            is_handoff: true
        }
    );

    // Original author Neon is Idle but hit failure streak >= 2 -> force handoff to Hydrogen.
    slots[0].state = SlotRunState::Idle;
    let streak_handoff = select_slot_for_rework(
        Some("coder-neon"),
        2,
        &slots,
        SameRoleDispatchMode::PriorityWithOverflow,
    );
    assert_eq!(
        streak_handoff,
        SlotSelection::Selected {
            slot_id: "coder-hydrogen".to_string(),
            is_handoff: true
        }
    );
}

#[test]
fn workspace_path_lock_prevents_overlapping_parallel_edits() {
    assert!(!paths_conflict(
        &["src/auth/".to_string()],
        &["src/billing/".to_string()]
    ));
    assert!(paths_conflict(
        &["src/auth/".to_string()],
        &["src/auth/login.rs".to_string()]
    ));
    assert!(paths_conflict(
        &["src\\reconcile\\mod.rs".to_string()],
        &["src/reconcile/".to_string()]
    ));
    assert!(paths_conflict(&[], &["src/auth/".to_string()]));
    assert!(paths_conflict(&["*".to_string()], &["src/auth/".to_string()]));

    let t1 = make_task("T1", &["src/core/"], &[], TaskStatus::Coding);
    let t2_disjoint = make_task("T2", &["src/export/"], &[], TaskStatus::Pending);
    let t3_overlap = make_task("T3", &["src/core/state.rs"], &[], TaskStatus::Pending);
    let t4_dep = make_task("T4", &["tests/"], &["T1"], TaskStatus::Pending);
    let all = vec![t1, t2_disjoint.clone(), t3_overlap.clone(), t4_dep.clone()];

    assert_eq!(has_active_path_conflict(&t2_disjoint, &all), None);
    assert_eq!(
        has_active_path_conflict(&t3_overlap, &all).as_deref(),
        Some("T1")
    );
    assert!(!dependencies_satisfied(&t4_dep, &all));
}
