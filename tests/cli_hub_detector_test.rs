use agy_conductor::billing::SlotUsageStats;
use agy_conductor::cli_hub::{
    default_cli_providers, detect_all_cli_providers, CliProviderId, CliSupportTier,
};
use agy_conductor::ipc::{UiCommand, UiEvent};
use agy_conductor::storage::store::{AppStorage, PersistedConfig};
use serde_json::Value;
use std::fs;

#[test]
fn cli_provider_ids_and_persisted_config_roundtrip_with_legacy_compat() {
    let all = CliProviderId::all();
    assert_eq!(all.len(), 4);

    for (provider, expected_json, expected_cmd) in [
        (CliProviderId::Agy, "\"agy\"", "agy"),
        (CliProviderId::Codex, "\"codex\"", "codex"),
        (CliProviderId::ClaudeCode, "\"claude_code\"", "claude"),
        (CliProviderId::OpenCode, "\"opencode\"", "opencode"),
    ] {
        let ser = serde_json::to_string(&provider).expect("serialize provider");
        assert_eq!(ser, expected_json);
        let de: CliProviderId = serde_json::from_str(&ser).expect("deserialize provider");
        assert_eq!(de, provider);
        assert_eq!(provider.command_name(), expected_cmd);
        assert!(!provider.display_name().is_empty());
        assert!(!provider.tagline().is_empty());
    }

    let temp = std::env::temp_dir().join(format!(
        "agy-clihub-cfg-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let storage = AppStorage::with_base_dir(temp.clone());
    storage.ensure_dirs().unwrap();

    // 旧版 V2 config.json 缺少 active_cli_provider 时应无缝默认回退至 Agy
    let legacy_v2 = r#"{
        "prefer_gateway_models": true,
        "last_workspace": "D:/work/demo",
        "dispatch_mode": "PriorityWithOverflow",
        "permission_mode": "AcceptEdits",
        "enable_sandbox": false,
        "max_rework_rounds": 8,
        "slots": []
    }"#;
    fs::write(storage.config_path(), legacy_v2).unwrap();
    let loaded_legacy = storage.load_config();
    assert_eq!(loaded_legacy.active_cli_provider, CliProviderId::Agy);

    // 切换并持久化 ClaudeCode 后重读保持一致
    let mut cfg = PersistedConfig::default();
    cfg.active_cli_provider = CliProviderId::ClaudeCode;
    storage.save_config(&cfg).unwrap();
    let reloaded = storage.load_config();
    assert_eq!(reloaded.active_cli_provider, CliProviderId::ClaudeCode);

    let _ = fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn detect_all_cli_providers_returns_four_entries_with_agy_active() {
    let defaults = default_cli_providers();
    assert_eq!(defaults.len(), 4);
    assert_eq!(defaults[0].id, CliProviderId::Agy);
    assert_eq!(defaults[0].support_tier, CliSupportTier::ActiveOrchestrator);

    let detected = detect_all_cli_providers(None, Some("0.1.99")).await;
    assert_eq!(detected.len(), 4);
    assert_eq!(detected[0].id, CliProviderId::Agy);
    assert_eq!(detected[0].support_tier, CliSupportTier::ActiveOrchestrator);
    assert_eq!(detected[0].version_hint.as_deref(), Some("0.1.99"));
    assert_eq!(detected[1].id, CliProviderId::Codex);
    assert_eq!(detected[2].id, CliProviderId::ClaudeCode);
    assert_eq!(detected[3].id, CliProviderId::OpenCode);
}

#[test]
fn v3_ui_command_and_event_contracts_serialize_cleanly() {
    let c1: UiCommand =
        serde_json::from_str(r#"{"cmd":"select_cli_provider","provider":"codex"}"#)
            .expect("select_cli_provider");
    assert!(matches!(
        c1,
        UiCommand::SelectCliProvider {
            provider: CliProviderId::Codex
        }
    ));

    let c2: UiCommand =
        serde_json::from_str(r#"{"cmd":"refresh_cli_providers"}"#).expect("refresh_cli_providers");
    assert!(matches!(c2, UiCommand::RefreshCliProviders));

    let c3: UiCommand = serde_json::from_str(
        r#"{"cmd":"launch_provider_external_cli","provider":"claude_code"}"#,
    )
    .expect("launch_provider_external_cli");
    assert!(matches!(
        c3,
        UiCommand::LaunchProviderExternalCli {
            provider: CliProviderId::ClaudeCode
        }
    ));

    let ev_cli: Value = serde_json::to_value(UiEvent::CliProvidersUpdated {
        active_provider: CliProviderId::OpenCode,
        providers: default_cli_providers(),
    })
    .expect("serialize CliProvidersUpdated");
    assert_eq!(ev_cli["type"], "cli_providers_updated");
    assert_eq!(ev_cli["active_provider"], "opencode");
    assert_eq!(ev_cli["providers"].as_array().unwrap().len(), 4);

    let mut usage = SlotUsageStats::for_model("[Gateway] gemini-4-neon-xhigh");
    usage.total_tokens = 12_345;
    usage.cost_usd = 0.0418;
    let ev_usage: Value = serde_json::to_value(UiEvent::SlotUsageUpdated {
        slot_id: "coder-neon".to_string(),
        usage,
    })
    .expect("serialize SlotUsageUpdated");
    assert_eq!(ev_usage["type"], "slot_usage_updated");
    assert_eq!(ev_usage["slot_id"], "coder-neon");
    assert_eq!(ev_usage["usage"]["total_tokens"], 12_345);
    assert_eq!(ev_usage["usage"]["rate_label"], "Neon $3/$15/M");
}
