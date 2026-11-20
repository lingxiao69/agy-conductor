use agy_conductor::agy::models::{
    build_default_roster, parse_models_output, ModelFamily, ReasoningTier,
};
use agy_conductor::agy::runner::{AgentTurnRequest, RealAgyRunner};
use agy_conductor::agy::stream_parser::{
    extract_structured_json, is_quota_error_text, parse_stream_json_line, ParsedStreamEvent,
};
use agy_conductor::mcp_bridge::stdio_server::{handle_rpc_request, McpBridgeConfig};
use agy_conductor::orchestrator::types::{PermissionMode, RoleKind};
use serde_json::json;
use std::path::PathBuf;

const SAMPLE_AGY_MODELS_STDOUT: &str = "\
Fetching available models...\n\
gemini-4-neon-high\tGemini 4 Neon (High)\n\
gemini-4-neon-xhigh\tGemini 4 Neon (XHigh)\n\
gemini-4-carbon-high\tGemini 4 Carbon (High)\n\
gemini-4-hydrogen-high\tGemini 4 Hydrogen (High)\n\
gemini-4-hydrogen-xhigh\tGemini 4 Hydrogen (XHigh)\n\
gemini-4-argon-medium\tGemini 4 Argon (Medium)\n\
gemini-3.8-flash-low\tGemini 3.8 Flash (Low)\n\
gemini-3.1-pro-high\tGemini 3.1 Pro (High)\n\
[Gateway] gemini-4-carbon-high\t[Gateway] gemini-4-carbon-high\n\
[Gateway] gemini-4-hydrogen-xhigh\t[Gateway] gemini-4-hydrogen-xhigh\n\
[Gateway] gemini-4-neon-xhigh\t[Gateway] gemini-4-neon-xhigh\n\
[Gateway] gemini-4-neon-xhigh\t[Gateway] gemini-4-neon-xhigh\n\
";

#[test]
fn parses_gateway_and_official_models_with_family_tier_and_dedup() {
    let models = parse_models_output(SAMPLE_AGY_MODELS_STDOUT);
    // Duplicate [Gateway] gemini-4-neon-xhigh must be deduplicated; header line ignored.
    assert_eq!(models.len(), 11);

    let gw_neon = models
        .iter()
        .find(|m| m.id == "[Gateway] gemini-4-neon-xhigh")
        .expect("gateway neon xhigh should exist");
    assert!(gw_neon.is_gateway);
    assert_eq!(gw_neon.family, ModelFamily::Neon);
    assert_eq!(gw_neon.tier, ReasoningTier::XHigh);
    assert_eq!(gw_neon.clean_slug, "gemini-4-neon-xhigh");

    let off_carbon = models
        .iter()
        .find(|m| m.id == "gemini-4-carbon-high")
        .expect("official carbon high should exist");
    assert!(!off_carbon.is_gateway);
    assert_eq!(off_carbon.family, ModelFamily::Carbon);
    assert_eq!(off_carbon.tier, ReasoningTier::High);

    let argon = models
        .iter()
        .find(|m| m.id == "gemini-4-argon-medium")
        .expect("argon medium should exist");
    assert_eq!(argon.family, ModelFamily::Argon);
    assert_eq!(argon.tier, ReasoningTier::Medium);

    let flash = models
        .iter()
        .find(|m| m.id == "gemini-3.8-flash-low")
        .expect("flash low should exist");
    assert_eq!(flash.family, ModelFamily::Flash);
    assert_eq!(flash.tier, ReasoningTier::Low);

    let pro = models
        .iter()
        .find(|m| m.id == "gemini-3.1-pro-high")
        .expect("pro high should exist");
    assert_eq!(pro.family, ModelFamily::Pro);
    assert_eq!(pro.tier, ReasoningTier::High);
}

#[test]
fn default_roster_respects_gateway_vs_official_preference() {
    let models = parse_models_output(SAMPLE_AGY_MODELS_STDOUT);

    let gw_roster = build_default_roster(&models, true);
    assert_eq!(gw_roster.len(), 4);
    assert_eq!(gw_roster[0].role, RoleKind::Architect);
    assert_eq!(gw_roster[0].model_id, "[Gateway] gemini-4-carbon-high");
    assert_eq!(gw_roster[1].role, RoleKind::Coder);
    assert_eq!(gw_roster[1].priority, 1);
    assert_eq!(gw_roster[1].model_id, "[Gateway] gemini-4-neon-xhigh");
    assert_eq!(gw_roster[2].role, RoleKind::Coder);
    assert_eq!(gw_roster[2].priority, 2);
    assert_eq!(gw_roster[2].model_id, "[Gateway] gemini-4-hydrogen-xhigh");
    assert_eq!(gw_roster[3].role, RoleKind::Reviewer);
    assert_eq!(gw_roster[3].model_id, "[Gateway] gemini-4-carbon-high");

    let off_roster = build_default_roster(&models, false);
    assert_eq!(off_roster[0].model_id, "gemini-4-carbon-high");
    assert_eq!(off_roster[1].model_id, "gemini-4-neon-xhigh");
    assert_eq!(off_roster[2].model_id, "gemini-4-hydrogen-xhigh");
}

#[test]
fn parses_stream_json_init_step_update_result_and_quota_errors() {
    let init_line = r#"{"event":"init","conversation_id":"conv-101","init":{"model":"[Gateway] gemini-4-neon-xhigh","cwd":"D:\\work\\demo"}}"#;
    match parse_stream_json_line(init_line) {
        Some(ParsedStreamEvent::Init {
            conversation_id,
            model,
        }) => {
            assert_eq!(conversation_id.as_deref(), Some("conv-101"));
            assert_eq!(model.as_deref(), Some("[Gateway] gemini-4-neon-xhigh"));
        }
        other => panic!("expected Init event, got {other:?}"),
    }

    let step_line = r#"{"event":"step_update","step_update":{"conversation_id":"conv-101","step_index":3,"state":"DONE","step_type":"tool","tool_name":"edit_file","tool_info":{"name":"edit_file","parameters":{"Path":"D:\\work\\demo\\src\\reconcile.rs"}}}}"#;
    match parse_stream_json_line(step_line) {
        Some(ParsedStreamEvent::StepUpdate {
            activity_summary,
            is_tool_step,
            ..
        }) => {
            assert!(is_tool_step);
            let text = activity_summary.expect("should summarize tool step");
            assert!(text.contains("src/reconcile.rs"), "got {text}");
        }
        other => panic!("expected StepUpdate event, got {other:?}"),
    }

    let result_line = r#"{"event":"result","result":{"conversation_id":"conv-101","status":"SUCCESS","response":"{\"status\":\"SUBMIT_FOR_REVIEW\",\"chat_message\":\"@carbon-rev done\",\"modified_files\":[\"src/lib.rs\"],\"self_check_result\":\"ok\"}","structured_output":{"status":"SUBMIT_FOR_REVIEW","chat_message":"@carbon-rev done","modified_files":["src/lib.rs"],"self_check_result":"ok"},"duration_seconds":12.4,"usage":{"input_tokens":500,"output_tokens":120,"thinking_tokens":80,"cache_read_tokens":200,"total_tokens":620}}}"#;
    match parse_stream_json_line(result_line) {
        Some(ParsedStreamEvent::Result(payload)) => {
            assert_eq!(payload.status, "SUCCESS");
            assert_eq!(payload.usage.total_tokens, 620);
            let json_val = extract_structured_json(&payload).expect("extract structured output");
            assert_eq!(json_val["status"], "SUBMIT_FOR_REVIEW");
        }
        other => panic!("expected Result event, got {other:?}"),
    }

    assert!(is_quota_error_text("HTTP 429 Too Many Requests"));
    assert!(is_quota_error_text("RESOURCE_EXHAUSTED: quota exceeded"));
    assert!(is_quota_error_text("Error: Out of credits for model"));
    assert!(!is_quota_error_text("Compilation failed on line 12"));
}

#[test]
fn runner_builds_safe_stream_json_args_without_cli_prompt_flag() {
    let req = AgentTurnRequest {
        slot_id: "coder-neon".to_string(),
        model_id: "[Gateway] gemini-4-neon-xhigh".to_string(),
        workspace_dir: PathBuf::from("D:\\work\\demo"),
        schema_path: PathBuf::from("C:\\schemas\\coder.json"),
        prompt: "很长的中文提示词".to_string(),
        conversation_id: Some("conv-888".to_string()),
        permission_mode: PermissionMode::DangerouslySkipPermissions,
        enable_sandbox: true,
        timeout_secs: 600,
    };

    let args = RealAgyRunner::build_turn_args(&req);
    assert!(!args.contains(&"--print".to_string()));
    assert!(args.windows(2).any(|w| w == ["--input-format", "stream-json"]));
    assert!(args.windows(2).any(|w| w == ["--output-format", "stream-json"]));
    assert!(args.windows(2).any(|w| w == ["--model", "[Gateway] gemini-4-neon-xhigh"]));
    assert!(args.windows(2).any(|w| w == ["--conversation", "conv-888"]));
    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"--sandbox".to_string()));
    assert!(!args.contains(&"-p".to_string()));
}

#[test]
fn mcp_bridge_posts_and_reads_group_messages() {
    let temp_dir = std::env::temp_dir().join(format!(
        "agy-conductor-mcp-test-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let cfg = McpBridgeConfig {
        room_dir: temp_dir.clone(),
        default_alias: "neon-cli".to_string(),
    };

    let init = handle_rpc_request(
        &cfg,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
    )
    .expect("initialize returns response");
    assert_eq!(init["result"]["serverInfo"]["name"], "agy-conductor-hub");

    let post = handle_rpc_request(
        &cfg,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "hub_post_message",
                "arguments": { "content": "@carbon-rev 已完成手动微调" }
            }
        }),
    )
    .expect("post returns response");
    assert!(post["result"].get("isError").is_none());

    let read = handle_rpc_request(
        &cfg,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "hub_read_messages", "arguments": { "after_index": 0 } }
        }),
    )
    .expect("read returns response");
    let body_text = read["result"]["content"][0]["text"].as_str().unwrap();
    assert!(body_text.contains("@carbon-rev 已完成手动微调"));

    let _ = std::fs::remove_dir_all(&temp_dir);
}
