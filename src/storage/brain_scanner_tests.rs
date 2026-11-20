use crate::billing::SlotUsageStats;
use crate::orchestrator::types::{AgentSlot, RoleKind, SlotRunState};
use crate::storage::store::{AppStorage, PersistedConfig, RoomSnapshot};
use crate::storage::workspace_sessions::{
    discover_workspace_role_sessions, normalize_workspace_key, save_room_role_sessions,
    upsert_workspace_role_session, WorkspaceRoleSession, SESSION_SOURCE_AGY_BRAIN,
    SESSION_SOURCE_CONDUCTOR,
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

struct TempBrainEnv {
    root: PathBuf,
    brain_dir: PathBuf,
    storage: AppStorage,
}

impl TempBrainEnv {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "agy-brain-scan-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let storage_dir = root.join("conductor_data");
        let brain_dir = root.join("mock_agy_brain");
        fs::create_dir_all(&brain_dir).expect("create mock brain dir");
        let storage = AppStorage::with_base_dir(storage_dir);
        storage.ensure_dirs().expect("ensure temp storage dirs");
        Self {
            root,
            brain_dir,
            storage,
        }
    }

    fn write_transcript(&self, conv_id: &str, lines: &[String]) -> PathBuf {
        let log_dir = self
            .brain_dir
            .join(conv_id)
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&log_dir).expect("create transcript log dir");
        let path = log_dir.join("transcript.jsonl");
        fs::write(&path, lines.join("\n")).expect("write transcript.jsonl");
        path
    }
}

impl Drop for TempBrainEnv {
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

#[test]
fn discover_merges_global_rooms_config_and_agy_brain_transcripts_safely() {
    let env = TempBrainEnv::new("discover");
    let ws = "D:\\work\\alpha";

    upsert_workspace_role_session(
        &env.storage,
        WorkspaceRoleSession {
            workspace: "d:/work/alpha/".to_string(),
            workspace_key: normalize_workspace_key("d:/work/alpha/"),
            slot_id: "coder-neon".to_string(),
            alias: "neon".to_string(),
            display_name: "编码程序员 · neon".to_string(),
            role: RoleKind::Coder,
            model_id: "[Gateway] gemini-4-neon".to_string(),
            conversation_id: "conv-neon-old".to_string(),
            last_goal: "旧目标".to_string(),
            room_id: Some("room-init".to_string()),
            updated_at: "2099-11-18T10:00:00+08:00".to_string(),
            source: SESSION_SOURCE_CONDUCTOR.to_string(),
            context_tokens: 0,
            healthy_for_reuse: true,
            unhealthy_reason: None,
        },
    )
    .expect("seed global");

    let room_dir = env
        .storage
        .create_room_dir("room-hist-1")
        .expect("create room");
    AppStorage::save_room_snapshot(
        &room_dir,
        &RoomSnapshot {
            room_id: "room-hist-1".to_string(),
            workspace: ws.to_string(),
            goal: "实现终端实时流追踪".to_string(),
            phase: "全部验收通过".to_string(),
            tasks: Vec::new(),
            updated_at: "2099-11-20T11:00:00+08:00".to_string(),
            total_tokens: 0,
            total_cost_usd: 0.0,
            slot_usages: Vec::new(),
        },
    )
    .expect("save snap");
    save_room_role_sessions(
        &room_dir,
        &[WorkspaceRoleSession {
            workspace: String::new(),
            workspace_key: String::new(),
            slot_id: "coder-neon".to_string(),
            alias: "neon".to_string(),
            display_name: "编码程序员 · Neon".to_string(),
            role: RoleKind::Coder,
            model_id: "[Gateway] gemini-4-neon-xhigh".to_string(),
            conversation_id: "conv-neon-latest".to_string(),
            last_goal: String::new(),
            room_id: None,
            updated_at: "2099-11-20T11:00:00+08:00".to_string(),
            source: SESSION_SOURCE_CONDUCTOR.to_string(),
            context_tokens: 0,
            healthy_for_reuse: true,
            unhealthy_reason: None,
        }],
    )
    .expect("save room sessions");

    let mut cfg = PersistedConfig::default();
    cfg.last_workspace = "D:/Work/Alpha".to_string();
    let mut hydro_slot = make_slot("coder-hydrogen", "hydrogen", RoleKind::Coder, 2);
    hydro_slot.conversation_id = Some("conv-hydro-cfg".to_string());
    cfg.slots = vec![
        make_slot("arch-carbon", "carbon-arch", RoleKind::Architect, 1),
        make_slot("coder-neon", "neon", RoleKind::Coder, 1),
        hydro_slot,
        make_slot("rev-carbon", "carbon-rev", RoleKind::Reviewer, 1),
    ];
    env.storage.save_config(&cfg).expect("save config");

    fs::write(
        env.root.join("settings.json"),
        r#"{"api_key":"SECRET_DO_NOT_READ"}"#,
    )
    .expect("write dummy outer file");

    env.write_transcript(
        "brain-arch-001",
        &[json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "created_at": "2099-11-20T12:00:00+08:00",
            "content": "你是 AgyConductor 研发群组的【总架构规划师 (Architect)】。\n请勘察当前项目目录 `D:\\work\\alpha`。\n\n## 团队成员（按优先级数字从小到大调度）\n- @carbon-arch | 总架构师 · Carbon | 角色: 总架构师 | 优先级: P1 | 模型: [Gateway] gemini-4-carbon-high\n\n## 用户长期目标\n构建跨会话记忆索引"
        })
        .to_string()],
    );

    env.write_transcript(
        "brain-coder5-002",
        &[
            json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "created_at": "2099-11-20T12:30:00+08:00",
                "content": "<USER_REQUEST>\n你是 AgyConductor 研发群组的【编码程序员 (Coder)】：编码程序员 #5 (@coder-5，优先级 P1)。\n\n## 项目长期目标\n支持识别本地角色会话\n\n## 架构契约与工程红线\n说明：识别 `【总架构规划师 (Architect)】` 与 `你是专职代码审查员` 特征。"
            })
            .to_string(),
            json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "created_at": "2099-11-20T12:30:05+08:00",
                "model": "gemini-4-neon-xhigh",
                "tool_calls": [{"name": "read_file", "args": {"Path": "\"D:\\\\work\\\\alpha\\\\Cargo.toml\""}}]
            })
            .to_string(),
        ],
    );

    env.write_transcript(
        "brain-rev-003",
        &[
            json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "created_at": "2099-11-20T12:45:00+08:00",
                "content": "你是专职代码审查员，只能读取文件、查看 `git diff`。\n\n## 项目长期目标\n验收会话识别模块\n\n## 待审查子任务：T2 — 存储\n提交程序员：@neon"
            })
            .to_string(),
            json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "created_at": "2099-11-20T12:45:10+08:00",
                "tool_calls": [{"name": "read_file", "args": {"Path": "\"D:\\\\work\\\\alpha\\\\src\\\\lib.rs\""}}]
            })
            .to_string(),
        ],
    );

    env.write_transcript(
        "brain-wrong-prefix",
        &[json!({
            "step_index": 0,
            "type": "USER_INPUT",
            "created_at": "2099-11-20T13:00:00+08:00",
            "content": "你是 AgyConductor 研发群组的【总架构规划师 (Architect)】。\n请勘察当前项目目录 `D:\\work\\alpha-v2`。"
        })
        .to_string()],
    );

    let mut late_lines: Vec<String> = (0..9)
        .map(|i| {
            json!({"step_index": i, "type": "USER_INPUT", "content": "你是专职代码审查员"})
                .to_string()
        })
        .collect();
    late_lines.push(
        json!({"step_index": 9, "type": "GENERIC", "content": "D:/work/alpha/src/main.rs"})
            .to_string(),
    );
    env.write_transcript("brain-beyond-8-lines", &late_lines);

    let discovered =
        discover_workspace_role_sessions(&env.storage, "d:/work/alpha/", Some(&env.brain_dir));

    assert_eq!(discovered.len(), 5, "discovered: {discovered:#?}");
    assert_eq!(discovered[0].role, RoleKind::Architect);
    assert_eq!(discovered[0].conversation_id, "brain-arch-001");
    assert_eq!(discovered[0].slot_id, "arch-carbon");
    assert_eq!(discovered[0].source, SESSION_SOURCE_AGY_BRAIN);
    assert_eq!(discovered[0].last_goal, "构建跨会话记忆索引");

    assert_eq!(discovered[1].role, RoleKind::Coder);
    assert_eq!(discovered[1].conversation_id, "brain-coder5-002");
    assert_eq!(discovered[1].alias, "coder-5");
    assert_eq!(discovered[1].display_name, "编码程序员 #5");

    let neon_rec = discovered
        .iter()
        .find(|s| s.alias == "neon")
        .expect("neon session");
    assert_eq!(neon_rec.conversation_id, "conv-neon-latest");
    assert_eq!(neon_rec.last_goal, "实现终端实时流追踪");
    assert_eq!(neon_rec.room_id.as_deref(), Some("room-hist-1"));
    assert!(!discovered
        .iter()
        .any(|s| s.conversation_id == "conv-neon-old"));
    assert!(discovered
        .iter()
        .any(|s| s.conversation_id == "conv-hydro-cfg"));

    let rev_rec = discovered.last().expect("reviewer session");
    assert_eq!(rev_rec.role, RoleKind::Reviewer);
    assert_eq!(rev_rec.conversation_id, "brain-rev-003");
    assert_eq!(rev_rec.slot_id, "rev-carbon");

    assert!(!discovered
        .iter()
        .any(|s| s.conversation_id == "brain-wrong-prefix"));
    assert!(!discovered
        .iter()
        .any(|s| s.conversation_id == "brain-beyond-8-lines"));

    let other_ws =
        discover_workspace_role_sessions(&env.storage, "D:/work/alpha-v2", Some(&env.brain_dir));
    assert_eq!(other_ws.len(), 1);
    assert_eq!(other_ws[0].conversation_id, "brain-wrong-prefix");
}

#[test]
fn brain_scanner_handles_32kb_multibyte_truncation_without_utf8_error() {
    let env = TempBrainEnv::new("utf8-bound");
    let padding = "中".repeat(12_000);
    let raw_line = format!(
        r#"{{"step_index":0,"type":"USER_INPUT","content":"你是 AgyConductor 研发群组的【编码程序员 (Coder)】：极速员 (@fast-coder，优先级 P1)。项目 `\\?\D:\Work\BigRepo` {padding}"}}"#
    );
    assert!(raw_line.len() > 32 * 1024);
    env.write_transcript("conv-huge-line", &[raw_line]);

    let found =
        discover_workspace_role_sessions(&env.storage, "D:/work/bigrepo", Some(&env.brain_dir));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].conversation_id, "conv-huge-line");
    assert_eq!(found[0].role, RoleKind::Coder);
    assert_eq!(found[0].alias, "fast-coder");
}
