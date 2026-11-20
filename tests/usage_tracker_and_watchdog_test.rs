use agy_conductor::agy::live_log::LiveStreamKind;
use agy_conductor::agy::resolver::{find_in_path, AgyLaunchStrategy, ResolvedAgyRuntime};
use agy_conductor::agy::runner::{AgentTurnRequest, AgyRunner, RealAgyRunner};
use agy_conductor::agy::stream_parser::{extract_live_stream_lines, TokenUsage};
use agy_conductor::agy::usage_tracker::TurnTranscriptUsageTracker;
use agy_conductor::orchestrator::types::PermissionMode;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use tokio::sync::{mpsc, watch};
use tokio::time::Duration;

fn write_conv_transcript(brain_dir: &std::path::Path, conv_id: &str, content: &str) -> PathBuf {
    let dir = brain_dir
        .join(conv_id)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&dir).expect("create brain log dir");
    let path = dir.join("transcript.jsonl");
    fs::write(&path, content).expect("write transcript");
    path
}

#[test]
fn transcript_usage_tracker_starts_from_offset_and_reconciles_without_double_count() {
    let temp = std::env::temp_dir().join(format!(
        "agy-tracker-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let brain_dir = temp.join("brain");

    // 预置旧历史轮次（50k 输入 + 10k 缓存），沿用旧会话时绝不能重复计入本轮！
    let old_history = format!(
        "{}\n{}\n",
        json!({
            "step_index": 0,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "usage": { "input_tokens": 30000, "cache_read_tokens": 10000, "output_tokens": 1500, "thinking_tokens": 2500 }
        }),
        json!({
            "step_index": 1,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "usage": { "input_tokens": 20000, "cache_read_tokens": 0, "output_tokens": 500, "thinking_tokens": 500 }
        }),
    );
    let transcript_path = write_conv_transcript(&brain_dir, "conv-resumed-1", &old_history);

    let mut tracker = TurnTranscriptUsageTracker::new(Some(brain_dir), Some("conv-resumed-1"));
    assert!(
        tracker.poll_incremental().is_none(),
        "沿用旧会话初始偏移处不应把历史步骤计为本轮新增消耗"
    );
    assert!(!tracker.has_any_usage());

    // 追加一条完整新步骤 + 一条未写完的半行 JSON（不含换行符）
    {
        let mut f = OpenOptions::new()
            .append(true)
            .open(&transcript_path)
            .expect("open transcript for append");
        let step_new = json!({
            "step_index": 2,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "usage": { "input_tokens": 4000, "cache_read_tokens": 8000, "output_tokens": 300, "thinking_tokens": 700 }
        });
        writeln!(f, "{step_new}").unwrap();
        write!(f, "{{\"step_index\":3,\"usage\":{{\"input_tokens\":999").unwrap();
    }

    let (u1, peak1) = tracker
        .poll_incremental()
        .expect("complete appended line should be tracked");
    assert_eq!(u1.input_tokens, 4_000);
    assert_eq!(u1.cache_read_tokens, 8_000);
    assert_eq!(u1.output_tokens, 300);
    assert_eq!(u1.thinking_tokens, 700);
    assert_eq!(u1.total_tokens, 13_000);
    assert_eq!(peak1, 12_000);

    // 补全半行 JSON 并以换行结束
    {
        let mut f = OpenOptions::new()
            .append(true)
            .open(&transcript_path)
            .expect("open transcript for second append");
        writeln!(
            f,
            ",\"cache_read_tokens\":1000,\"output_tokens\":200,\"thinking_tokens\":100}}}}"
        )
        .unwrap();
    }

    let (u2, peak2) = tracker
        .poll_incremental()
        .expect("completed second line should now be tracked");
    assert_eq!(u2.input_tokens, 4_999);
    assert_eq!(u2.cache_read_tokens, 9_000);
    assert_eq!(u2.output_tokens, 500);
    assert_eq!(u2.thinking_tokens, 800);
    assert_eq!(peak2, 12_000);

    // 权威 Result 对齐：逐项取最大值，不双倍累加
    let authoritative = TokenUsage {
        input_tokens: 5_200,
        cache_read_tokens: 9_000,
        output_tokens: 600,
        thinking_tokens: 800,
        total_tokens: 15_600,
    };
    let (final_u, _) = tracker.reconcile_final_usage(&authoritative);
    assert_eq!(final_u.input_tokens, 5_200);
    assert_eq!(final_u.cache_read_tokens, 9_000);
    assert_eq!(final_u.output_tokens, 600);
    assert_eq!(final_u.thinking_tokens, 800);
    assert_eq!(final_u.total_tokens, 15_600);

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn live_stream_extractor_emits_checkpoint_and_agent_response_lines() {
    let checkpoint_line = r#"{"event":"step_update","step_update":{"step_index":0,"step_type":"checkpoint","state":"RUNNING"}}"#;
    let lines = extract_live_stream_lines(checkpoint_line);
    assert!(
        lines
            .iter()
            .any(|(kind, text)| *kind == LiveStreamKind::System && text.contains("检查点")),
        "checkpoint step should emit visible System line, got: {lines:?}"
    );

    let agent_resp_line = r#"{"event":"step_update","step_update":{"step_index":4,"step_type":"agent_response","state":"DONE"}}"#;
    let resp_lines = extract_live_stream_lines(agent_resp_line);
    assert!(
        resp_lines
            .iter()
            .any(|(kind, text)| *kind == LiveStreamKind::System && text.contains("结构化")),
        "agent_response without text should emit visible System line, got: {resp_lines:?}"
    );
}

#[tokio::test]
async fn first_progress_watchdog_recovers_stalled_resumed_session_by_retrying_fresh() {
    let Some(node_exe) = find_in_path(&["node.exe", "node"]) else {
        // 环境无 node 时跳过子进程测试
        return;
    };

    let temp = std::env::temp_dir().join(format!(
        "agy-watchdog-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&temp).expect("create temp dir");
    let script_path = temp.join("fake_agy_cli.cjs");
    let schema_path = temp.join("schema.json");
    fs::write(&schema_path, "{}").unwrap();

    // 模拟真实 V2 卡死场景：
    // - 携带 `--conversation` 时仅输出 init + checkpoint 后永久卡住；
    // - 降级为全新会话（无 `--conversation`）后正常返回 thinking + SUCCESS result。
    let fake_script = r#"
const hasConv = process.argv.includes('--conversation');
process.stdin.resume();
process.stdin.on('end', () => {
  if (hasConv) {
    console.log(JSON.stringify({ event: 'init', conversation_id: 'old-stalled-conv', init: { model: 'gemini-4-carbon-high' } }));
    console.log(JSON.stringify({ event: 'step_update', step_update: { conversation_id: 'old-stalled-conv', step_index: 0, state: 'RUNNING', step_type: 'checkpoint' } }));
    setTimeout(() => process.exit(0), 30000);
  } else {
    console.log(JSON.stringify({ event: 'init', conversation_id: 'fresh-conv-999', init: { model: 'gemini-4-carbon-high' } }));
    console.log(JSON.stringify({ event: 'step_update', step_update: { conversation_id: 'fresh-conv-999', step_index: 1, state: 'DONE', step_type: 'thinking', thinking: '轻量新会话秒速启动' } }));
    console.log(JSON.stringify({
      event: 'result',
      result: {
        conversation_id: 'fresh-conv-999',
        status: 'SUCCESS',
        response: '{"ok":true}',
        structured_output: { ok: true },
        duration_seconds: 0.25,
        usage: { input_tokens: 1200, cache_read_tokens: 0, output_tokens: 200, thinking_tokens: 100, total_tokens: 1500 }
      }
    }));
    process.exit(0);
  }
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
        .with_first_progress_watchdog(Duration::from_millis(350))
        .with_brain_dir_override(temp.join("empty-brain"));

    let (act_tx, mut act_rx) = mpsc::unbounded_channel();
    let (_cancel_tx, cancel_rx) = watch::channel(false);

    let req = AgentTurnRequest {
        slot_id: "arch-carbon".to_string(),
        model_id: "[Gateway] gemini-4-carbon-high".to_string(),
        workspace_dir: temp.clone(),
        schema_path,
        prompt: "请勘察项目并输出计划".to_string(),
        conversation_id: Some("old-stalled-conv".to_string()),
        permission_mode: PermissionMode::AcceptEdits,
        enable_sandbox: false,
        timeout_secs: 30,
    };

    let resp = runner
        .run_turn(req, act_tx, cancel_rx)
        .await
        .expect("watchdog must seamlessly recover by retrying with fresh session");

    assert!(resp.switched_to_fresh_session);
    assert_eq!(resp.conversation_id.as_deref(), Some("fresh-conv-999"));
    assert_eq!(resp.structured_json["ok"], true);
    assert_eq!(resp.usage.total_tokens, 1500);

    let mut saw_fallback_activity = false;
    while let Ok(act) = act_rx.try_recv() {
        if act.summary.contains("全新轻量会话") {
            saw_fallback_activity = true;
        }
    }
    assert!(saw_fallback_activity, "should report fallback retry activity");

    let _ = fs::remove_dir_all(&temp);
}
