use crate::billing::SlotUsageStats;
use crate::orchestrator::types::{AgentSlot, RoleKind, SlotRunState};
use crate::storage::store::{AppStorage, PersistedConfig};
use crate::storage::workspace_sessions::{
    load_all_workspace_role_sessions, normalize_workspace_key, read_room_role_sessions,
    resolve_reused_sessions_for_slots, save_room_role_sessions, upsert_workspace_role_session,
    upsert_workspace_role_sessions, WorkspaceRoleSession, SESSION_SOURCE_CONDUCTOR,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

struct TempWsEnv {
    root: PathBuf,
    storage: AppStorage,
}

impl TempWsEnv {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "agy-ws-sessions-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let storage = AppStorage::with_base_dir(root.join("conductor_data"));
        storage.ensure_dirs().expect("ensure temp storage dirs");
        Self { root, storage }
    }
}

impl Drop for TempWsEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn make_slot(slot_id: &str, alias: &str, role: RoleKind, priority: u8) -> AgentSlot {
    AgentSlot {
        slot_id: slot_id.to_string(),
        alias: alias.to_string(),
        display_name: format!("{} · {alias}", role.label_zh()),
        model_id: format!("model-{alias}"),
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

fn make_session(
    workspace: &str,
    slot_id: &str,
    alias: &str,
    role: RoleKind,
    conv_id: &str,
    updated_at: &str,
) -> WorkspaceRoleSession {
    WorkspaceRoleSession {
        workspace: workspace.to_string(),
        workspace_key: normalize_workspace_key(workspace),
        slot_id: slot_id.to_string(),
        alias: alias.to_string(),
        display_name: format!("{} · {alias}", role.label_zh()),
        role,
        model_id: format!("[Gateway] gemini-4-{alias}"),
        conversation_id: conv_id.to_string(),
        last_goal: "修复终端黑屏并沿用会话".to_string(),
        room_id: Some("room-init".to_string()),
        updated_at: updated_at.to_string(),
        source: SESSION_SOURCE_CONDUCTOR.to_string(),
        context_tokens: 0,
        healthy_for_reuse: true,
        unhealthy_reason: None,
    }
}

fn assert_utf8_no_bom(path: &Path) {
    let bytes = fs::read(path).expect("read persisted file");
    assert!(
        !bytes.starts_with(&[0xEF, 0xBB, 0xBF]),
        "file must be UTF-8 without BOM: {}",
        path.display()
    );
    String::from_utf8(bytes).expect("file must be valid UTF-8");
}

#[test]
fn normalize_workspace_key_handles_slashes_trailing_verbatim_and_case() {
    let expected = "d:/work/agy-conductor";
    assert_eq!(normalize_workspace_key("D:\\Work\\Agy-Conductor"), expected);
    assert_eq!(
        normalize_workspace_key("  d:/work/agy-conductor/  "),
        expected
    );
    assert_eq!(
        normalize_workspace_key("D:\\work\\\\agy-conductor\\\\"),
        expected
    );
    assert_eq!(
        normalize_workspace_key("\\\\?\\D:\\Work\\Agy-Conductor\\"),
        expected
    );
    assert_eq!(normalize_workspace_key("   "), "");
    assert_eq!(normalize_workspace_key("/"), "/");
}

#[test]
fn persisted_config_defaults_reuse_role_sessions_to_true() {
    let env = TempWsEnv::new("cfg-reuse");
    assert!(PersistedConfig::default().reuse_role_sessions);

    let legacy = r#"{
        "custom_agy_path": null,
        "prefer_gateway_models": true,
        "last_workspace": "D:/work/demo",
        "dispatch_mode": "PriorityWithOverflow",
        "permission_mode": "AcceptEdits",
        "enable_sandbox": false,
        "max_rework_rounds": 8,
        "slots": []
    }"#;
    fs::write(env.storage.config_path(), legacy).expect("write legacy config");
    assert!(env.storage.load_config().reuse_role_sessions);

    let mut cfg = env.storage.load_config();
    cfg.reuse_role_sessions = false;
    env.storage.save_config(&cfg).expect("save config");
    assert!(!env.storage.load_config().reuse_role_sessions);
}

#[test]
fn global_and_room_session_persistence_deduplicate_and_write_utf8_no_bom() {
    let env = TempWsEnv::new("persist");

    let s_old = make_session(
        "D:\\Work\\Demo\\",
        "coder-neon",
        "neon",
        RoleKind::Coder,
        "conv-old",
        "2026-11-19T10:00:00+08:00",
    );
    upsert_workspace_role_session(&env.storage, &s_old).expect("upsert old");

    let s_new = make_session(
        "d:/work/demo",
        "coder-neon",
        "neon",
        RoleKind::Coder,
        "conv-new",
        "2026-11-20T11:00:00+08:00",
    );
    let s_hydro = make_session(
        "D:/work/demo",
        "coder-hydrogen",
        "hydrogen",
        RoleKind::Coder,
        "conv-hydro",
        "2026-11-20T10:30:00+08:00",
    );
    let s_other_ws = make_session(
        "D:/work/other-repo",
        "coder-neon",
        "neon",
        RoleKind::Coder,
        "conv-other-ws",
        "2026-11-20T12:00:00+08:00",
    );

    upsert_workspace_role_sessions(
        &env.storage,
        vec![s_new.clone(), s_hydro.clone(), s_other_ws],
    )
    .expect("batch upsert");

    let global_path = env.storage.base_dir().join("workspace_sessions.json");
    assert_utf8_no_bom(&global_path);

    let all = load_all_workspace_role_sessions(&env.storage);
    assert_eq!(all.len(), 3);
    let demo_sessions: Vec<_> = all
        .iter()
        .filter(|s| s.workspace_key == "d:/work/demo")
        .collect();
    assert_eq!(demo_sessions.len(), 2);
    assert!(demo_sessions.iter().any(|s| s.conversation_id == "conv-new"));
    assert!(!demo_sessions.iter().any(|s| s.conversation_id == "conv-old"));

    let room_dir = env.storage.create_room_dir("room-test-01").expect("room dir");
    let s_arch = make_session(
        "D:/work/demo",
        "arch-carbon",
        "carbon-arch",
        RoleKind::Architect,
        "conv-arch",
        "2026-11-20T09:00:00+08:00",
    );
    save_room_role_sessions(&room_dir, &[s_hydro, s_arch, s_new]).expect("save room sessions");
    assert_utf8_no_bom(&room_dir.join("sessions.json"));

    let room_loaded = read_room_role_sessions(&room_dir);
    assert_eq!(room_loaded.len(), 3);
    assert_eq!(room_loaded[0].role, RoleKind::Architect);
    assert_eq!(room_loaded[1].conversation_id, "conv-new");
    assert_eq!(room_loaded[2].conversation_id, "conv-hydro");
}

#[test]
fn resolve_reused_sessions_honors_explicit_slot_alias_and_role_fallback_without_collision() {
    let slots = vec![
        make_slot("arch-1", "carbon-arch", RoleKind::Architect, 1),
        make_slot("coder-1", "neon", RoleKind::Coder, 1),
        make_slot("coder-2", "hydrogen", RoleKind::Coder, 2),
        make_slot("coder-3", "argon", RoleKind::Coder, 3),
        make_slot("coder-4", "pro", RoleKind::Coder, 4),
        make_slot("rev-1", "carbon-rev", RoleKind::Reviewer, 1),
    ];

    let available = vec![
        make_session(
            "D:/work/demo",
            "architect",
            "architect",
            RoleKind::Architect,
            "conv-arch-fallback",
            "2026-11-20T12:00:00+08:00",
        ),
        make_session(
            "D:/work/demo",
            "coder-legacy",
            "legacy",
            RoleKind::Coder,
            "conv-coder-fallback",
            "2026-11-20T14:00:00+08:00",
        ),
        make_session(
            "D:/work/demo",
            "neon-brain-id",
            "NEON",
            RoleKind::Coder,
            "conv-neon-alias",
            "2026-11-20T11:00:00+08:00",
        ),
        make_session(
            "D:/work/demo",
            "coder-2",
            "old-alias",
            RoleKind::Coder,
            "conv-coder2-slot",
            "2026-11-20T10:00:00+08:00",
        ),
        make_session(
            "D:/work/demo",
            "rev-1",
            "carbon-rev",
            RoleKind::Reviewer,
            "conv-rev-exact",
            "2026-11-20T12:30:00+08:00",
        ),
    ];

    let mut explicit = HashMap::new();
    explicit.insert("coder-4".to_string(), "conv-manual-override".to_string());
    explicit.insert("rev-1".to_string(), String::new());

    let resolved = resolve_reused_sessions_for_slots(&slots, &available, &explicit);

    assert_eq!(resolved["arch-1"].conversation_id, "conv-arch-fallback");
    assert_eq!(resolved["coder-1"].conversation_id, "conv-neon-alias");
    assert_eq!(resolved["coder-2"].conversation_id, "conv-coder2-slot");
    assert_eq!(resolved["coder-3"].conversation_id, "conv-coder-fallback");
    assert_eq!(resolved["coder-4"].conversation_id, "conv-manual-override");
    assert!(
        !resolved.contains_key("rev-1"),
        "explicit empty session id should opt out of reuse"
    );
}
