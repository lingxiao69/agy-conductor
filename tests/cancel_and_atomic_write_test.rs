use agy_conductor::agy::json_extract::{extract_structured_json, is_quota_error_text};
use agy_conductor::agy::resolver::{find_in_path, AgyLaunchStrategy, ResolvedAgyRuntime};
use agy_conductor::agy::runner::{AgentTurnRequest, AgyRunner, RealAgyRunner};
use agy_conductor::agy::stream_parser::{StreamResultPayload, TokenUsage};
use agy_conductor::mcp_bridge::stdio_server::{
    read_next_rpc_message, FramingMode, MAX_MCP_FRAME_BYTES,
};
use agy_conductor::orchestrator::types::PermissionMode;
use agy_conductor::storage::store::atomic_write_text;
use std::fs;
use std::io::Cursor;
use tokio::sync::{mpsc, watch};
use tokio::time::{timeout, Duration};

#[test]
fn atomic_write_overwrites_existing_file_cleanly_without_tmp_leftover() {
    let temp = std::env::temp_dir().join(format!(
        "agy-atomic-write-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&temp).unwrap();
    let target = temp.join("config.json");

    atomic_write_text(&target, r#"{"version":1,"msg":"初始内容"}"#).expect("first atomic write");
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        r#"{"version":1,"msg":"初始内容"}"#
    );

    // 多次连续覆盖，验证 Windows MoveFileExW 原地原子替换且无 .tmp 残余
    for round in 2..=6 {
        let payload = format!(r#"{{"version":{round},"msg":"第 {round} 次原子覆盖"}}"#);
        atomic_write_text(&target, &payload).expect("overwrite atomic write");
        assert_eq!(fs::read_to_string(&target).unwrap(), payload);
    }

    let leftovers: Vec<_> = fs::read_dir(&temp)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "expected zero .tmp residue, found: {leftovers:?}");

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn quota_429_boundary_and_reverse_json_extract_and_mcp_frame_limit() {
    // 429 边界匹配：避免源码行号/端口号/Issue 编号误判为限流
    assert!(!is_quota_error_text("Compilation failed at src/main.rs line 429"));
    assert!(!is_quota_error_text("listening on port 429 failed"));
    assert!(!is_quota_error_text("Fixed issue #429 in parser"));
    assert!(is_quota_error_text("HTTP 429 Too Many Requests"));
    assert!(is_quota_error_text("upstream returned status: 429"));
    assert!(is_quota_error_text("Error 429: quota exhausted"));

    // 逆向提取最后一个 JSON 代码块（不被前文示例 JSON 劫持）
    let mixed = r#"
前文示例格式如下：
```json
{"action": "EXAMPLE_ONLY", "tasks": []}
```
经过完整分析，最终正式决策：
```json
{"action": "FINAL_ACCEPT", "chat_message": "验收通过"}
```
"#;
    let payload = StreamResultPayload {
        conversation_id: None,
        status: "SUCCESS".to_string(),
        response: mixed.to_string(),
        structured_output: None,
        error: None,
        finished_reason: None,
        duration_seconds: 1.0,
        usage: TokenUsage::default(),
    };
    let parsed = extract_structured_json(&payload).expect("extract last json block");
    assert_eq!(parsed["action"], "FINAL_ACCEPT");

    // MCP 帧长度上限保护 (>16 MiB 立即拒收，防 OOM)
    let oversized_len = MAX_MCP_FRAME_BYTES + 1024;
    let mut bad_stream = Cursor::new(format!("Content-Length: {oversized_len}\r\n\r\n{{}}"));
    let err = read_next_rpc_message(&mut bad_stream)
        .expect_err("oversized MCP frame must be rejected");
    assert!(err.contains("安全上限") || err.contains("超过上限"), "unexpected error: {err}");

    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let mut good_stream = Cursor::new(format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    let (msg, mode) = read_next_rpc_message(&mut good_stream)
        .expect("valid MCP frame")
        .expect("message present");
    assert_eq!(mode, FramingMode::ContentLength);
    assert_eq!(msg, body);
}

#[tokio::test]
async fn dropped_cancel_sender_does_not_busy_spin_or_abort_running_turn() {
    let Some(node_exe) = find_in_path(&["node.exe", "node"]) else {
        return;
    };

    let temp = std::env::temp_dir().join(format!(
        "agy-cancel-drop-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&temp).unwrap();
    let script_path = temp.join("fake_ok_cli.cjs");
    let schema_path = temp.join("schema.json");
    fs::write(&schema_path, "{}").unwrap();

    let fake_script = r#"
process.stdin.resume();
process.stdin.on('end', () => {
  console.log(JSON.stringify({ event: 'init', conversation_id: 'conv-drop-ok', init: { model: 'gemini-4-carbon-high' } }));
  setTimeout(() => {
    console.log(JSON.stringify({
      event: 'result',
      result: {
        conversation_id: 'conv-drop-ok',
        status: 'SUCCESS',
        response: '{"done":true}',
        structured_output: { done: true },
        duration_seconds: 0.15,
        usage: { input_tokens: 500, cache_read_tokens: 0, output_tokens: 100, thinking_tokens: 0, total_tokens: 600 }
      }
    }));
    process.exit(0);
  }, 150);
});
"#;
    fs::write(&script_path, fake_script).unwrap();

    let runtime = ResolvedAgyRuntime {
        strategy: AgyLaunchStrategy::GatewayNodeWrapper {
            node_exe,
            launch_mjs: script_path.clone(),
            underlying_agy_exe: None,
            cmd_script: script_path.clone(),
        },
        display_path: script_path,
    };

    let runner = RealAgyRunner::new(runtime, None)
        .with_brain_dir_override(temp.join("empty-brain"));
    let (act_tx, _act_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    // 立即丢弃发送端：修复前 select! 中 `_ = cancel_rx.changed()` 会瞬间返回 Err 陷入 100% CPU 死循环！
    drop(cancel_tx);

    let req = AgentTurnRequest {
        slot_id: "arch-test".to_string(),
        model_id: "[Gateway] gemini-4-carbon-high".to_string(),
        workspace_dir: temp.clone(),
        schema_path,
        prompt: "ping".to_string(),
        conversation_id: None,
        permission_mode: PermissionMode::AcceptEdits,
        enable_sandbox: false,
        timeout_secs: 30,
    };

    let resp = timeout(Duration::from_secs(8), runner.run_turn(req, act_tx, cancel_rx))
        .await
        .expect("turn must finish promptly without busy spin")
        .expect("turn must succeed cleanly when cancel sender dropped");

    assert_eq!(resp.structured_json["done"], true);
    assert_eq!(resp.usage.total_tokens, 600);

    let _ = fs::remove_dir_all(&temp);
}
