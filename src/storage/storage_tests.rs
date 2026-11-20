use crate::billing::SlotUsageStats;
use crate::orchestrator::types::{
    AgentSlot, GroupChatMessage, MessageBadge, PermissionMode, ReviewIssue, RoleKind,
    SameRoleDispatchMode, SlotRunState, TaskItem, TaskStatus,
};
use crate::storage::history::{
    delete_history_room, export_history_markdown, list_history_rooms, load_history_room,
    render_history_markdown, suggested_markdown_filename, validate_room_id,
};
use crate::storage::presets::{apply_preset_to_config, delete_preset, save_preset};
use crate::storage::store::{AppStorage, PersistedConfig, RoomSnapshot};
use std::fs;
use std::path::PathBuf;

struct TempStorageGuard {
    root: PathBuf,
    storage: AppStorage,
}

impl TempStorageGuard {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "agy-conductor-test-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let storage = AppStorage::with_base_dir(root.clone());
        storage.ensure_dirs().expect("ensure temp storage dirs");
        Self { root, storage }
    }
}

impl Drop for TempStorageGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn sample_slot(slot_id: &str, role: RoleKind, model_id: &str, priority: u8) -> AgentSlot {
    AgentSlot {
        slot_id: slot_id.to_string(),
        alias: slot_id.to_string(),
        display_name: format!("成员-{slot_id}"),
        model_id: model_id.to_string(),
        role,
        priority,
        enabled: true,
        state: SlotRunState::Idle,
        conversation_id: None,
        session_reused: false,
        current_activity: None,
        consecutive_failures: 0,
        usage: SlotUsageStats::default(),
    }
}

fn sample_task(task_id: &str, title: &str, status: TaskStatus) -> TaskItem {
    TaskItem {
        task_id: task_id.to_string(),
        title: title.to_string(),
        description: "测试子任务说明".to_string(),
        owned_paths: vec!["src/storage".to_string()],
        depends_on: Vec::new(),
        preferred_coder_alias: Some("neon".to_string()),
        acceptance_criteria: vec!["cargo test --lib 通过".to_string()],
        status,
        assigned_coder_slot_id: Some("coder-1".to_string()),
        assigned_coder_label: Some("Neon (P1)".to_string()),
        last_coder_slot_id: Some("coder-1".to_string()),
        rework_count: 1,
        arch_arbitration_count: 0,
        coder_failure_streak: 0,
        last_coder_summary: Some("完成历史记录模块".to_string()),
        last_modified_files: vec!["src/storage/history.rs".to_string()],
        last_self_check: Some("cargo test --lib ok".to_string()),
        last_review_issues: vec![ReviewIssue {
            severity: "minor".to_string(),
            file_path: "src/storage/history.rs".to_string(),
            line_hint: Some("L42".to_string()),
            detail: "补充路径校验注释".to_string(),
        }],
        last_review_note: Some("验收通过".to_string()),
        blocker_reason: None,
    }
}

fn sample_message(id: &str, sender: &str, content: &str, badge: MessageBadge) -> GroupChatMessage {
    GroupChatMessage {
        id: id.to_string(),
        timestamp: "12:30:00".to_string(),
        sender_slot_id: Some("coder-1".to_string()),
        sender_alias: "neon".to_string(),
        sender_display: sender.to_string(),
        sender_role: Some(RoleKind::Coder),
        priority: Some(1),
        is_user: false,
        is_system: false,
        content: content.to_string(),
        badge,
        task_id: Some("T1".to_string()),
        modified_files: vec!["src/storage/history.rs".to_string()],
        duration_seconds: Some(12.4),
        tool_steps: Some(4),
        issues: Vec::new(),
    }
}

#[test]
fn legacy_config_without_presets_deserializes_cleanly() {
    let env = TempStorageGuard::new("legacy-cfg");
    let legacy_json = r#"{
        "custom_agy_path": "C:/tools/agy.exe",
        "prefer_gateway_models": false,
        "last_workspace": "D:/work/demo",
        "dispatch_mode": "StrictPrimaryFailover",
        "permission_mode": "DangerouslySkipPermissions",
        "enable_sandbox": true,
        "max_rework_rounds": 6,
        "slots": []
    }"#;
    fs::write(env.storage.config_path(), legacy_json).expect("write legacy config");

    let cfg = env.storage.load_config();
    assert_eq!(cfg.custom_agy_path.as_deref(), Some("C:/tools/agy.exe"));
    assert!(!cfg.prefer_gateway_models);
    assert_eq!(cfg.last_workspace, "D:/work/demo");
    assert_eq!(cfg.dispatch_mode, SameRoleDispatchMode::StrictPrimaryFailover);
    assert_eq!(cfg.permission_mode, PermissionMode::DangerouslySkipPermissions);
    assert!(cfg.enable_sandbox);
    assert_eq!(cfg.max_rework_rounds, 6);
    assert!(cfg.presets.is_empty());
    assert!(cfg.active_preset_id.is_none());
}

#[test]
fn preset_save_sanitize_overwrite_apply_delete_and_persist() {
    let env = TempStorageGuard::new("presets");
    let mut cfg = PersistedConfig::default();

    assert!(save_preset(&mut cfg, "   ", vec![], SameRoleDispatchMode::PriorityWithOverflow).is_err());
    assert!(
        save_preset(
            &mut cfg,
            "空编队",
            vec![],
            SameRoleDispatchMode::PriorityWithOverflow
        )
        .is_err()
    );

    let mut dirty_slot = sample_slot("coder-1", RoleKind::Coder, "gemini-flash", 0);
    dirty_slot.state = SlotRunState::Working;
    dirty_slot.conversation_id = Some("conv-dirty-1".to_string());
    dirty_slot.current_activity = Some("运行测试中".to_string());
    dirty_slot.consecutive_failures = 3;

    let high_prio_slot = sample_slot("rev-1", RoleKind::Reviewer, "gemini-pro", 150);

    let (saved, overwritten) = save_preset(
        &mut cfg,
        "  日常极速组  ",
        vec![dirty_slot.clone(), high_prio_slot],
        SameRoleDispatchMode::PriorityWithOverflow,
    )
    .expect("save new preset");
    assert!(!overwritten);
    assert_eq!(saved.name, "日常极速组");
    assert_eq!(cfg.active_preset_id.as_deref(), Some(saved.preset_id.as_str()));
    assert_eq!(saved.slots[0].priority, 1);
    assert_eq!(saved.slots[0].state, SlotRunState::Idle);
    assert!(saved.slots[0].conversation_id.is_none());
    assert!(saved.slots[0].current_activity.is_none());
    assert_eq!(saved.slots[0].consecutive_failures, 0);
    assert_eq!(saved.slots[1].priority, 99);

    let (updated, overwritten_again) = save_preset(
        &mut cfg,
        "日常极速组",
        vec![sample_slot("arch-1", RoleKind::Architect, "carbon-xhigh", 2)],
        SameRoleDispatchMode::StrictPrimaryFailover,
    )
    .expect("overwrite same-name preset");
    assert!(overwritten_again);
    assert_eq!(updated.preset_id, saved.preset_id);
    assert_eq!(cfg.presets.len(), 1);
    assert_eq!(updated.dispatch_mode, SameRoleDispatchMode::StrictPrimaryFailover);

    let (second, _) = save_preset(
        &mut cfg,
        "架构精审组",
        vec![sample_slot("arch-2", RoleKind::Architect, "carbon-pro", 1)],
        SameRoleDispatchMode::StrictPrimaryFailover,
    )
    .expect("save second preset");
    assert_eq!(cfg.presets.len(), 2);

    env.storage.save_config(&cfg).expect("persist config");
    let mut reloaded = env.storage.load_config();
    assert_eq!(reloaded.presets.len(), 2);
    assert_eq!(reloaded.active_preset_id.as_deref(), Some(second.preset_id.as_str()));

    let applied = apply_preset_to_config(&mut reloaded, &saved.preset_id).expect("apply preset");
    assert_eq!(applied.name, "日常极速组");
    assert_eq!(reloaded.active_preset_id.as_deref(), Some(saved.preset_id.as_str()));
    assert_eq!(reloaded.slots.len(), 1);
    assert_eq!(reloaded.slots[0].slot_id, "arch-1");

    // 删除非激活预设时保留 active_preset_id
    let removed_inactive = delete_preset(&mut reloaded, &second.preset_id).expect("delete inactive");
    assert_eq!(removed_inactive.preset_id, second.preset_id);
    assert_eq!(reloaded.active_preset_id.as_deref(), Some(saved.preset_id.as_str()));

    // 删除当前激活预设时清空 active_preset_id
    let removed_active = delete_preset(&mut reloaded, &saved.preset_id).expect("delete active");
    assert_eq!(removed_active.preset_id, saved.preset_id);
    assert!(reloaded.active_preset_id.is_none());
    assert!(reloaded.presets.is_empty());
    assert!(delete_preset(&mut reloaded, "preset-missing").is_err());
}

#[test]
fn validate_room_id_blocks_path_traversal_and_invalid_names() {
    assert!(validate_room_id("room-0123abcd").is_ok());
    assert!(validate_room_id("cli_2026.1").is_ok());

    for bad in [
        "",
        "   ",
        " room-1",
        ".",
        "..",
        ".hidden",
        "room.",
        "room..1",
        "../outside",
        "..\\outside",
        "sub/room",
        "sub\\room",
        "C:room",
        "room\0id",
        "room name",
    ] {
        assert!(validate_room_id(bad).is_err(), "expected invalid room_id: {bad:?}");
    }
}

#[test]
fn history_list_load_delete_and_board_only_fallback() {
    let env = TempStorageGuard::new("history");

    // 1. 较旧房间（含状态快照 + 2 条任务 + 1 条消息）
    let old_dir = env.storage.create_room_dir("room-old").expect("create old");
    AppStorage::save_room_snapshot(
        &old_dir,
        &RoomSnapshot {
            room_id: "room-old".to_string(),
            workspace: "D:/work/alpha".to_string(),
            goal: "实现旧版本目标".to_string(),
            phase: "全部验收通过".to_string(),
            tasks: vec![
                sample_task("T1", "基础模块", TaskStatus::Approved),
                sample_task("T2", "集成测试", TaskStatus::PendingReview),
            ],
            updated_at: "2026-11-18T09:00:00+08:00".to_string(),
            total_tokens: 0,
            total_cost_usd: 0.0,
            slot_usages: Vec::new(),
        },
    )
    .expect("save old snap");
    AppStorage::append_board_message(
        &old_dir,
        &sample_message("m1", "编码程序员 · Neon", "提交 T1", MessageBadge::Submit),
    )
    .expect("append old msg");

    // 2. 较新房间
    let new_dir = env.storage.create_room_dir("room-new").expect("create new");
    AppStorage::save_room_snapshot(
        &new_dir,
        &RoomSnapshot {
            room_id: "room-new".to_string(),
            workspace: "D:/work/beta".to_string(),
            goal: "实现历史抽屉与预设切换".to_string(),
            phase: "编码与审查中".to_string(),
            tasks: vec![sample_task("T1", "存储与 IPC | 契约", TaskStatus::Approved)],
            updated_at: "2026-11-20T12:00:00+08:00".to_string(),
            total_tokens: 0,
            total_cost_usd: 0.0,
            slot_usages: Vec::new(),
        },
    )
    .expect("save new snap");
    AppStorage::append_board_message(
        &new_dir,
        &sample_message("m2", "编码程序员 · Neon", "实现完成 @carbon-rev", MessageBadge::Submit),
    )
    .expect("append new msg");
    AppStorage::append_board_message(
        &new_dir,
        &sample_message("m3", "审查员 · Carbon", "验收通过", MessageBadge::Approved),
    )
    .expect("append new msg 2");

    // 3. 仅有 board.jsonl 的房间
    let board_only_dir = env
        .storage
        .create_room_dir("room-board-only")
        .expect("create board-only");
    AppStorage::append_board_message(
        &board_only_dir,
        &sample_message(
            "m4",
            "轻舟协调器",
            "已启动长期目标自动协同：验证仅日志恢复（目录：D:/work/demo）",
            MessageBadge::Info,
        ),
    )
    .expect("append board-only msg");

    // 4. 空目录与杂项文件（应被忽略）
    let _empty = env.storage.create_room_dir("room-empty").expect("create empty");
    fs::write(env.storage.rooms_dir().join("stray.txt"), "ignore").expect("write stray");

    let list = list_history_rooms(&env.storage).expect("list history");
    assert_eq!(list.len(), 3);
    assert!(!list.iter().any(|r| r.room_id == "room-empty"));

    let new_pos = list.iter().position(|r| r.room_id == "room-new").unwrap();
    let old_pos = list.iter().position(|r| r.room_id == "room-old").unwrap();
    assert!(new_pos < old_pos, "newer room should appear before older room");

    let old_summary = &list[old_pos];
    assert_eq!(old_summary.task_count, 2);
    assert_eq!(old_summary.approved_task_count, 1);
    assert_eq!(old_summary.message_count, 1);

    let board_only = load_history_room(&env.storage, "room-board-only").expect("load board-only");
    assert_eq!(board_only.goal, "验证仅日志恢复");
    assert_eq!(board_only.phase, "仅对话记录");
    assert_eq!(board_only.messages.len(), 1);

    // 路径穿越删除与加载均应被阻断，且不能影响上级目录文件
    let sentinel = env.root.join("keep-me.txt");
    fs::write(&sentinel, "safe").expect("write sentinel");
    assert!(load_history_room(&env.storage, "../keep-me.txt").is_err());
    assert!(delete_history_room(&env.storage, "../keep-me.txt").is_err());
    assert!(sentinel.exists());

    // 正常删除旧记录
    delete_history_room(&env.storage, "room-old").expect("delete room-old");
    assert!(!old_dir.exists());
    let after = list_history_rooms(&env.storage).expect("list after delete");
    assert_eq!(after.len(), 2);
    assert!(!after.iter().any(|r| r.room_id == "room-old"));
}

#[test]
fn history_markdown_render_and_atomic_export() {
    let env = TempStorageGuard::new("export-md");
    let dir = env.storage.create_room_dir("room-md-01").expect("create room");
    AppStorage::save_room_snapshot(
        &dir,
        &RoomSnapshot {
            room_id: "room-md-01".to_string(),
            workspace: "D:/work/agy-conductor".to_string(),
            goal: "支持导出清晰的 Markdown 历史报告".to_string(),
            phase: "全部验收通过".to_string(),
            tasks: vec![sample_task("T1", "存储层 | 导出", TaskStatus::Approved)],
            updated_at: "2026-11-20T12:45:00+08:00".to_string(),
            total_tokens: 0,
            total_cost_usd: 0.0,
            slot_usages: Vec::new(),
        },
    )
    .expect("save snap");
    AppStorage::append_board_message(
        &dir,
        &sample_message("m1", "编码程序员 · Neon", "已完成导出逻辑", MessageBadge::Submit),
    )
    .expect("append msg");

    let detail = load_history_room(&env.storage, "room-md-01").expect("load detail");
    assert_eq!(suggested_markdown_filename(&detail), "agy-history-room-md-01.md");

    let md = render_history_markdown(&detail);
    assert!(md.contains("# AgyConductor 项目协同历史报告"));
    assert!(md.contains("`room-md-01`"));
    assert!(md.contains("支持导出清晰的 Markdown 历史报告"));
    assert!(md.contains("存储层 \\| 导出"));
    assert!(md.contains("已完成导出逻辑"));
    assert!(md.contains("src/storage/history.rs:L42"));

    let out_path = env.root.join("exports").join("report.md");
    export_history_markdown(&detail, &out_path).expect("export md");
    let raw_bytes = fs::read(&out_path).expect("read exported md");
    assert!(!raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "must be UTF-8 without BOM");
    let exported = String::from_utf8(raw_bytes).expect("valid utf8");
    assert_eq!(exported, md);
}
