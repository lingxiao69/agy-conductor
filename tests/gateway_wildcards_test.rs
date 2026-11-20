use agy_conductor::agy::gateway_wildcards::{
    prepare_gateway_session_settings, sanitize_wildcard_model_id,
};
use agy_conductor::agy::models::{build_default_roster, parse_models_output};
use agy_conductor::agy::resolver::{AgyLaunchStrategy, ResolvedAgyRuntime};
use agy_conductor::agy::runner::{AgentTurnRequest, RealAgyRunner};
use agy_conductor::agy::terminal::{build_interactive_agy_command_vec, TerminalLaunchOptions};
use agy_conductor::billing::SlotUsageStats;
use agy_conductor::ipc::state::AppCoreState;
use agy_conductor::orchestrator::types::{
    AgentSlot, PermissionMode, RoleKind, SameRoleDispatchMode, SlotRunState,
};
use agy_conductor::storage::presets::RosterPreset;
use agy_conductor::storage::store::AppStorage;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

fn unique_temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agy-conductor-gw-wc-{tag}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_slot(id: &str, alias: &str, role: RoleKind, model_id: &str) -> AgentSlot {
    AgentSlot {
        slot_id: id.to_string(),
        alias: alias.to_string(),
        display_name: format!("{} · {}", role.label_zh(), alias),
        model_id: model_id.to_string(),
        role,
        priority: 1,
        enabled: true,
        state: SlotRunState::Idle,
        conversation_id: None,
        session_reused: false,
        current_activity: None,
        consecutive_failures: 0,
        usage: SlotUsageStats::for_model(model_id),
    }
}

#[test]
fn expands_gateway_wildcards_using_official_models_and_eliminates_all_asterisks() {
    let stdout = "\
Fetching available models...\n\
gemini-4-carbon-high\tGemini 4 Carbon (High)\n\
gemini-4-carbon-medium\tGemini 4 Carbon (Medium)\n\
gemini-4-carbon-low\tGemini 4 Carbon (Low)\n\
gemini-4-neon-xhigh\tGemini 4 Neon (XHigh)\n\
gemini-4-neon-high\tGemini 4 Neon (High)\n\
gemini-4-neon-medium\tGemini 4 Neon (Medium)\n\
gemini-4-neon-low\tGemini 4 Neon (Low)\n\
gemini-4-hydrogen-xhigh\tGemini 4 Hydrogen (XHigh)\n\
gemini-4-hydrogen-high\tGemini 4 Hydrogen (High)\n\
gemini-4-hydrogen-medium\tGemini 4 Hydrogen (Medium)\n\
gemini-4-hydrogen-low\tGemini 4 Hydrogen (Low)\n\
gemini-4-argon-high\tGemini 4 Argon (High)\n\
gemini-4-argon-medium\tGemini 4 Argon (Medium)\n\
gemini-4-argon-low\tGemini 4 Argon (Low)\n\
[Gateway] gemini-3.8-flash-high\t[Gateway] gemini-3.8-flash-high\n\
[Gateway] gemini-4-argon\t[Gateway] gemini-4-argon\n\
[Gateway] gemini-4-argon*\t[Gateway] gemini-4-argon*\n\
[Gateway] gemini-4-carbon*\t[Gateway] gemini-4-carbon*\n\
[Gateway] gemini-4-hydrogen*\t[Gateway] gemini-4-hydrogen*\n\
[Gateway] gemini-4-neon*\t[Gateway] gemini-4-neon*\n\
";

    let models = parse_models_output(stdout);
    assert!(
        models.iter().all(|m| !m.id.contains('*') && !m.clean_slug.contains('*')),
        "parsed models must contain zero wildcard entries: {models:?}"
    );

    for expected in [
        "[Gateway] gemini-4-carbon-high",
        "[Gateway] gemini-4-carbon-medium",
        "[Gateway] gemini-4-carbon-low",
        "[Gateway] gemini-4-neon-xhigh",
        "[Gateway] gemini-4-neon-high",
        "[Gateway] gemini-4-hydrogen-xhigh",
        "[Gateway] gemini-4-argon-high",
        "[Gateway] gemini-3.8-flash-high",
    ] {
        assert!(
            models.iter().any(|m| m.id == expected && m.is_gateway),
            "expected expanded gateway model {expected} in {models:?}"
        );
    }

    let roster = build_default_roster(&models, true);
    assert_eq!(roster.len(), 4);
    assert!(roster.iter().all(|s| !s.model_id.contains('*')));
    assert_eq!(roster[0].model_id, "[Gateway] gemini-4-carbon-high");
    assert_eq!(roster[1].model_id, "[Gateway] gemini-4-neon-xhigh");
    assert_eq!(roster[2].model_id, "[Gateway] gemini-4-hydrogen-xhigh");
    assert_eq!(roster[3].model_id, "[Gateway] gemini-4-carbon-high");
}

#[test]
fn expands_gateway_wildcards_with_family_fallback_when_official_models_absent() {
    let stdout = "\
[Gateway] gemini-4-carbon*\t[Gateway] gemini-4-carbon*\n\
[Gateway] gemini-4-neon*\t[Gateway] gemini-4-neon*\n\
[Gateway] gemini-4-hydrogen*\t[Gateway] gemini-4-hydrogen*\n\
[Gateway] custom-lab-model*\t[Gateway] custom-lab-model*\n\
";
    let models = parse_models_output(stdout);
    assert!(models.iter().all(|m| !m.id.contains('*')));
    assert!(models.iter().any(|m| m.id == "[Gateway] gemini-4-carbon-high"));
    assert!(models.iter().any(|m| m.id == "[Gateway] gemini-4-carbon-low"));
    assert!(!models.iter().any(|m| m.id == "[Gateway] gemini-4-carbon-xhigh"));
    assert!(models.iter().any(|m| m.id == "[Gateway] gemini-4-neon-xhigh"));
    assert!(models.iter().any(|m| m.id == "[Gateway] gemini-4-hydrogen-xhigh"));
    assert!(models.iter().any(|m| m.id == "[Gateway] custom-lab-model"));
}

#[test]
fn sanitizes_dirty_wildcard_models_and_heals_persisted_config_and_presets() {
    assert_eq!(
        sanitize_wildcard_model_id("[Gateway] gemini-4-carbon*"),
        "[Gateway] gemini-4-carbon-high"
    );
    assert_eq!(
        sanitize_wildcard_model_id("[Gateway] gemini-4-neon*"),
        "[Gateway] gemini-4-neon-xhigh"
    );
    assert_eq!(
        sanitize_wildcard_model_id("[Gateway] gemini-4-hydrogen*"),
        "[Gateway] gemini-4-hydrogen-xhigh"
    );
    assert_eq!(
        sanitize_wildcard_model_id("[Gateway] gemini-4-argon*"),
        "[Gateway] gemini-4-argon-high"
    );
    assert_eq!(
        sanitize_wildcard_model_id("gemini-4-carbon*"),
        "gemini-4-carbon-high"
    );
    assert_eq!(
        sanitize_wildcard_model_id("[Gateway] gemini-4-neon-medium"),
        "[Gateway] gemini-4-neon-medium"
    );

    let temp = unique_temp_dir("config_heal");
    let storage = AppStorage::with_base_dir(temp.join("appdata"));
    storage.ensure_dirs().unwrap();

    let mut dirty = storage.load_config();
    dirty.slots = vec![
        make_slot("arch-1", "arch", RoleKind::Architect, "[Gateway] gemini-4-carbon*"),
        make_slot("coder-1", "neon", RoleKind::Coder, "[Gateway] gemini-4-neon*"),
    ];
    dirty.presets.push(RosterPreset {
        preset_id: "p1".to_string(),
        name: "污染预设".to_string(),
        dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
        slots: vec![make_slot(
            "coder-2",
            "hydrogen",
            RoleKind::Coder,
            "[Gateway] gemini-4-hydrogen*",
        )],
        updated_at: "2026-11-20T00:00:00+08:00".to_string(),
    });
    storage.save_config(&dirty).unwrap();

    let state = AppCoreState::new(storage.clone(), None, None);
    assert_eq!(state.config.slots[0].model_id, "[Gateway] gemini-4-carbon-high");
    assert_eq!(state.config.slots[1].model_id, "[Gateway] gemini-4-neon-xhigh");
    assert_eq!(
        state.config.presets[0].slots[0].model_id,
        "[Gateway] gemini-4-hydrogen-xhigh"
    );

    // Reload from disk to confirm self-healing was persisted
    let reloaded = storage.load_config();
    assert_eq!(reloaded.slots[0].model_id, "[Gateway] gemini-4-carbon-high");
    assert_eq!(reloaded.slots[1].model_id, "[Gateway] gemini-4-neon-xhigh");
    assert_eq!(
        reloaded.presets[0].slots[0].model_id,
        "[Gateway] gemini-4-hydrogen-xhigh"
    );

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn prepares_gateway_session_settings_with_thinking_level_rules_and_mcp_merge_readonly() {
    let temp = unique_temp_dir("session_settings");
    let mock_agy_settings = temp.join("agy-settings.json");
    let orig_json = serde_json::to_string_pretty(&json!({
        "modelProvider": "gemini",
        "customModelsConfig": {
            "customModels": {
                "gemini-3.8-flash-high": {
                    "label": "[Gateway] gemini-3.8-flash-high",
                    "modelName": "models/gemini-3.8-flash-high",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                    "maxTokens": 1048576,
                    "maxOutputTokens": 65536,
                    "thinkingLevel": "HIGH"
                },
                "gemini-4-carbon*": {
                    "label": "[Gateway] gemini-4-carbon*",
                    "modelName": "models/gemini-4-carbon*",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                    "maxTokens": 1048576,
                    "maxOutputTokens": 65536
                },
                "gemini-4-neon*": {
                    "label": "[Gateway] gemini-4-neon*",
                    "modelName": "models/gemini-4-neon*",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                    "maxTokens": 1048576,
                    "maxOutputTokens": 65536
                }
            }
        }
    }))
    .unwrap();
    fs::write(&mock_agy_settings, &orig_json).unwrap();

    let out_dir = temp.join("out");

    // 1. Concrete model already present in settings.json + no MCP -> returns None
    let existing = prepare_gateway_session_settings(
        "[Gateway] gemini-3.8-flash-high",
        &out_dir,
        "flash",
        None,
        Some(&mock_agy_settings),
    )
    .unwrap();
    assert!(existing.is_none());

    // 2. Non-gateway model + no MCP -> returns None
    let official = prepare_gateway_session_settings(
        "gemini-4-carbon-high",
        &out_dir,
        "off",
        None,
        Some(&mock_agy_settings),
    )
    .unwrap();
    assert!(official.is_none());

    // 3. Carbon High: must inject concrete modelName + thinkingLevel == "HIGH" + modelProvider + mcpServers,
    //    and strip wildcard keys from customModels.
    let mcp = json!({
        "agy-conductor-hub": {
            "command": "C:\\bin\\agy-conductor.exe",
            "args": ["mcp-stdio", "--room", "D:\\room", "--alias", "arch"]
        }
    });
    let carbon_path = prepare_gateway_session_settings(
        "[Gateway] gemini-4-carbon-high",
        &out_dir,
        "arch-carbon",
        Some(mcp),
        Some(&mock_agy_settings),
    )
    .unwrap()
    .expect("must generate session settings for expanded carbon-high");
    let carbon_val: Value =
        serde_json::from_str(&fs::read_to_string(&carbon_path).unwrap()).unwrap();
    assert_eq!(carbon_val["modelProvider"], "gemini");
    assert!(carbon_val["mcpServers"]["agy-conductor-hub"].is_object());
    let carbon_models = carbon_val["customModelsConfig"]["customModels"]
        .as_object()
        .unwrap();
    assert!(!carbon_models.contains_key("gemini-4-carbon*"));
    assert!(!carbon_models.contains_key("gemini-4-neon*"));
    assert!(carbon_models.contains_key("gemini-3.8-flash-high"));
    let carbon_entry = &carbon_models["gemini-4-carbon-high"];
    assert_eq!(carbon_entry["label"], "[Gateway] gemini-4-carbon-high");
    assert_eq!(carbon_entry["modelName"], "models/gemini-4-carbon-high");
    assert_eq!(carbon_entry["apiProvider"], "API_PROVIDER_GOOGLE_GEMINI");
    assert_eq!(carbon_entry["maxTokens"], 1_048_576);
    assert_eq!(carbon_entry["maxOutputTokens"], 65_536);
    assert_eq!(carbon_entry["thinkingLevel"], "HIGH");

    // 4. Neon XHigh: MUST NOT write thinkingLevel field (Go parser compatibility)
    let neon_path = prepare_gateway_session_settings(
        "[Gateway] gemini-4-neon-xhigh",
        &out_dir,
        "coder-neon",
        None,
        Some(&mock_agy_settings),
    )
    .unwrap()
    .expect("must generate session settings for expanded neon-xhigh");
    let neon_val: Value = serde_json::from_str(&fs::read_to_string(&neon_path).unwrap()).unwrap();
    let neon_entry = &neon_val["customModelsConfig"]["customModels"]["gemini-4-neon-xhigh"];
    assert_eq!(neon_entry["label"], "[Gateway] gemini-4-neon-xhigh");
    assert_eq!(neon_entry["modelName"], "models/gemini-4-neon-xhigh");
    assert!(
        neon_entry.get("thinkingLevel").is_none(),
        "XHIGH entry must omit thinkingLevel for Go compatibility: {neon_entry}"
    );

    // 5. Original mock settings.json MUST remain byte-for-byte untouched
    assert_eq!(fs::read_to_string(&mock_agy_settings).unwrap(), orig_json);

    // 6. Verify runner & terminal both sanitize wildcard model IDs before passing --model
    let schema_file = out_dir.join("coder.json");
    fs::write(&schema_file, "{}").unwrap();
    let req = AgentTurnRequest {
        slot_id: "coder-neon".to_string(),
        model_id: "[Gateway] gemini-4-neon*".to_string(),
        workspace_dir: temp.clone(),
        schema_path: schema_file,
        prompt: "hi".to_string(),
        conversation_id: None,
        permission_mode: PermissionMode::AcceptEdits,
        enable_sandbox: false,
        timeout_secs: 60,
    };
    let turn_args = RealAgyRunner::build_turn_args(&req);
    assert!(
        turn_args
            .windows(2)
            .any(|w| w == ["--model", "[Gateway] gemini-4-neon-xhigh"]),
        "runner must sanitize wildcard --model: {turn_args:?}"
    );

    let runtime = ResolvedAgyRuntime {
        strategy: AgyLaunchStrategy::DirectBinary {
            agy_exe: PathBuf::from("C:\\tools\\agy.exe"),
        },
        display_path: PathBuf::from("C:\\tools\\agy.exe"),
    };
    let dirty_slot = make_slot("arch-1", "arch", RoleKind::Architect, "[Gateway] gemini-4-carbon*");
    let cli_cmd = build_interactive_agy_command_vec(
        &runtime,
        Some(&dirty_slot),
        &TerminalLaunchOptions {
            workspace_dir: temp.clone(),
            room_dir: Some(out_dir.clone()),
            mount_mcp: true,
            attach_conversation: false,
            watch_live: false,
        },
    )
    .unwrap();
    assert!(
        cli_cmd
            .windows(2)
            .any(|w| w == ["--model", "[Gateway] gemini-4-carbon-high"]),
        "terminal must sanitize wildcard --model: {cli_cmd:?}"
    );
    assert!(
        cli_cmd.iter().any(|a| a == "--settings"),
        "terminal must inject unified --settings file: {cli_cmd:?}"
    );

    let _ = fs::remove_dir_all(&temp);
}
