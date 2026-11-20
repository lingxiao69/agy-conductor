use agy_conductor::billing::SlotUsageStats;
use agy_conductor::ipc::session_terminal_actions::{
    apply_session_reuse_for_slots_detailed, persist_all_active_slot_sessions,
};
use agy_conductor::orchestrator::types::{AgentSlot, RoleKind, SlotRunState};
use agy_conductor::storage::brain_health::{
    inspect_transcript_tail_health, MAX_HEALTHY_REUSE_CONTEXT_TOKENS,
};
use agy_conductor::storage::store::AppStorage;
use agy_conductor::storage::workspace_sessions::{
    discover_workspace_role_sessions, load_all_workspace_role_sessions, read_room_role_sessions,
    resolve_reused_sessions_for_slots,
};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn write_transcript(brain_dir: &std::path::Path, conv_id: &str, lines: &[String]) -> PathBuf {
    let dir = brain_dir
        .join(conv_id)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&dir).expect("create transcript dir");
    let path = dir.join("transcript.jsonl");
    fs::write(&path, lines.join("\n")).expect("write transcript");
    path
}

fn mk_slot(id: &str, alias: &str, role: RoleKind, priority: u8) -> AgentSlot {
    AgentSlot {
        slot_id: id.to_string(),
        alias: alias.to_string(),
        display_name: format!("{} · {alias}", role.label_zh()),
        model_id: "[Gateway] gemini-4-carbon-high".to_string(),
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

#[test]
fn brain_health_filters_bloated_and_canceled_sessions_from_auto_reuse() {
    let temp = std::env::temp_dir().join(format!(
        "agy-brain-health-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let brain_dir = temp.join("brain");
    let storage = AppStorage::with_base_dir(temp.join("appdata"));
    storage.ensure_dirs().unwrap();
    let ws = "D:\\work\\v3-health-demo";

    // 1. 健康的架构师会话（32k 上下文，正常 DONE）
    let healthy_path = write_transcript(
        &brain_dir,
        "conv-arch-healthy",
        &[
            json!({
                "step_index": 0,
                "type": "USER_INPUT",
                "created_at": "2099-11-20T10:00:00+08:00",
                "content": "你是 AgyConductor 研发群组的【总架构规划师 (Architect)】。\n请勘察当前项目目录 `D:\\work\\v3-health-demo`。\n## 用户长期目标\n实现轻舟 V3"
            })
            .to_string(),
            json!({
                "step_index": 1,
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2099-11-20T10:01:00+08:00",
                "usage": { "input_tokens": 12000, "cache_read_tokens": 20000, "output_tokens": 800, "thinking_tokens": 600 }
            })
            .to_string(),
        ],
    );

    // 2. 膨胀的架构师会话（254k > 120k，即使时间更新也必须拒绝自动沿用！）
    let bloated_path = write_transcript(
        &brain_dir,
        "conv-arch-bloated",
        &[
            json!({
                "step_index": 0,
                "type": "USER_INPUT",
                "created_at": "2099-11-20T12:00:00+08:00",
                "content": "你是 AgyConductor 研发群组的【总架构规划师 (Architect)】。\n请勘察当前项目目录 `D:\\work\\v3-health-demo`。\n## 用户长期目标\n超长旧对话"
            })
            .to_string(),
            json!({
                "step_index": 80,
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2099-11-20T12:30:00+08:00",
                "usage": { "input_tokens": 54000, "cache_read_tokens": 200000, "output_tokens": 1200, "thinking_tokens": 800 }
            })
            .to_string(),
        ],
    );

    // 3. 末尾 CANCELED 的编码员会话（仅 18k 上下文，但上次异常中断）
    let canceled_path = write_transcript(
        &brain_dir,
        "conv-coder-canceled",
        &[
            json!({
                "step_index": 0,
                "type": "USER_INPUT",
                "created_at": "2099-11-20T12:40:00+08:00",
                "content": "你是 AgyConductor 研发群组的【编码程序员 (Coder)】：编码程序员 (@neon，优先级 P1)。\n项目目录：`D:\\work\\v3-health-demo`"
            })
            .to_string(),
            json!({
                "step_index": 2,
                "type": "PLANNER_RESPONSE",
                "status": "CANCELED",
                "created_at": "2099-11-20T12:41:00+08:00",
                "usage": { "input_tokens": 10000, "cache_read_tokens": 8000, "output_tokens": 100, "thinking_tokens": 0 }
            })
            .to_string(),
        ],
    );

    let h_ok = inspect_transcript_tail_health(&healthy_path);
    assert!(h_ok.healthy_for_reuse);
    assert_eq!(h_ok.context_tokens, 32_000);
    assert!(h_ok.context_tokens <= MAX_HEALTHY_REUSE_CONTEXT_TOKENS);

    let h_bloated = inspect_transcript_tail_health(&bloated_path);
    assert!(!h_bloated.healthy_for_reuse);
    assert_eq!(h_bloated.context_tokens, 254_000);
    assert!(h_bloated.unhealthy_reason.as_deref().unwrap().contains("254k"));

    let h_canceled = inspect_transcript_tail_health(&canceled_path);
    assert!(!h_canceled.healthy_for_reuse);
    assert!(h_canceled
        .unhealthy_reason
        .as_deref()
        .unwrap()
        .contains("中断"));

    let discovered = discover_workspace_role_sessions(&storage, ws, Some(&brain_dir));
    assert_eq!(discovered.len(), 3);

    let mut slots = vec![
        mk_slot("arch-1", "arch", RoleKind::Architect, 1),
        mk_slot("coder-1", "neon", RoleKind::Coder, 1),
    ];

    let auto = resolve_reused_sessions_for_slots(&slots, &discovered, &HashMap::new());
    // 架构师必须跳过更新但膨胀的 254k 会话，选中健康的 32k 会话！
    assert_eq!(auto["arch-1"].conversation_id, "conv-arch-healthy");
    // 编码员唯一候选是 CANCELED，不得被自动强塞！
    assert!(!auto.contains_key("coder-1"));

    let outcome =
        apply_session_reuse_for_slots_detailed(&mut slots, &discovered, true, &HashMap::new());
    assert_eq!(outcome.reused.len(), 1);
    assert_eq!(outcome.reused[0].1, "conv-arch-healthy");
    assert_eq!(outcome.reused[0].2, 32_000);
    assert_eq!(outcome.skipped_unhealthy.len(), 1);
    assert!(outcome.skipped_unhealthy[0].0.contains("neon"));

    // 验证 Bug 2 根治：启动绑定后立即落盘 workspace_sessions.json 与 rooms/<id>/sessions.json
    let room_dir = storage.create_room_dir("room-persist-check").unwrap();
    slots[0].usage.peak_context_tokens = 32_000;
    persist_all_active_slot_sessions(
        &storage,
        ws,
        Some("room-persist-check"),
        Some(&room_dir),
        "验证启动落盘",
        &slots,
    );

    let ws_json_path = storage.base_dir().join("workspace_sessions.json");
    assert!(ws_json_path.is_file(), "workspace_sessions.json must exist immediately");
    let raw_bytes = fs::read(&ws_json_path).unwrap();
    assert!(!raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "UTF-8 no BOM");

    let persisted = load_all_workspace_role_sessions(&storage);
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].conversation_id, "conv-arch-healthy");
    assert_eq!(persisted[0].context_tokens, 32_000);
    assert!(persisted[0].healthy_for_reuse);

    let room_sessions = read_room_role_sessions(&room_dir);
    assert_eq!(room_sessions.len(), 1);
    assert_eq!(room_sessions[0].conversation_id, "conv-arch-healthy");

    let _ = fs::remove_dir_all(&temp);
}
