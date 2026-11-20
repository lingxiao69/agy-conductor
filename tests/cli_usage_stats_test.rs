use agy_conductor::billing::PricingCatalog;
use agy_conductor::storage::store::AppStorage;
use agy_conductor::storage::workspace_sessions::{
    upsert_workspace_role_session, WorkspaceRoleSession, SESSION_SOURCE_CONDUCTOR,
};
use agy_conductor::usage_stats::{
    collect_cli_usage_overview_with_roots, UsageRange, UsageScanRoots,
};
use chrono::Local;
use serde_json::json;
use std::fs;
use std::path::Path;

fn assert_close(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "{label}: expected {expected:.6}, got {actual:.6}"
    );
}

#[cfg(windows)]
fn create_mock_opencode_sqlite_db(db_path: &Path, created_ms: i64) {
    use std::ffi::{c_char, c_int, c_void, CString};
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };

    const SQLITE_OK: c_int = 0;
    const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
    const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;

    type FnOpenV2 = unsafe extern "C" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
    type FnCloseV2 = unsafe extern "C" fn(*mut c_void) -> c_int;
    type FnExec = unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *const c_void,
        *mut c_void,
        *mut *mut c_char,
    ) -> c_int;

    let wide: Vec<u16> = "winsqlite3.dll".encode_utf16().chain(std::iter::once(0)).collect();
    let module = unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    assert!(!module.is_null(), "winsqlite3.dll must be present on Windows");

    let open_fn: FnOpenV2 = unsafe {
        std::mem::transmute(
            GetProcAddress(module, b"sqlite3_open_v2\0".as_ptr()).expect("sqlite3_open_v2"),
        )
    };
    let close_fn: FnCloseV2 = unsafe {
        std::mem::transmute(
            GetProcAddress(module, b"sqlite3_close_v2\0".as_ptr()).expect("sqlite3_close_v2"),
        )
    };
    let exec_fn: FnExec = unsafe {
        std::mem::transmute(
            GetProcAddress(module, b"sqlite3_exec\0".as_ptr()).expect("sqlite3_exec"),
        )
    };

    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let c_path = CString::new(db_path.to_string_lossy().as_bytes()).unwrap();
    let mut db: *mut c_void = std::ptr::null_mut();
    let rc = unsafe {
        open_fn(
            c_path.as_ptr(),
            &mut db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
            std::ptr::null(),
        )
    };
    assert_eq!(rc, SQLITE_OK, "create mock opencode.db");

    let assistant_data = json!({
        "role": "assistant",
        "modelID": "gpt-6-sol",
        "time": { "created": created_ms },
        "tokens": {
            "input": 40_000,
            "output": 8_000,
            "reasoning": 2_000,
            "cache": { "read": 100_000, "write": 0 }
        }
    })
    .to_string();
    let user_data = json!({
        "role": "user",
        "modelID": "gpt-6-sol",
        "time": { "created": created_ms },
        "tokens": { "input": 999_999, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } }
    })
    .to_string();

    let sql = format!(
        "CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE account (id TEXT PRIMARY KEY, secret_token TEXT);
         INSERT INTO account VALUES ('acc1', 'DO_NOT_READ_SECRET');
         INSERT INTO message VALUES ('msg-oc-1', 'ses-oc-1', {created_ms}, '{}');
         INSERT INTO message VALUES ('msg-oc-user', 'ses-oc-1', {created_ms}, '{}');
         INSERT INTO session_message VALUES ('msg-oc-1', 'ses-oc-1', {created_ms}, '{}');",
        assistant_data.replace('\'', "''"),
        user_data.replace('\'', "''"),
        assistant_data.replace('\'', "''"),
    );
    let c_sql = CString::new(sql).unwrap();
    let exec_rc = unsafe {
        exec_fn(
            db,
            c_sql.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(exec_rc, SQLITE_OK, "populate mock opencode.db");
    unsafe {
        let _ = close_fn(db);
        let _ = FreeLibrary(module);
    }
}

#[test]
fn aggregates_all_four_clis_with_exact_semantics_dedup_and_incremental_cache() {
    let temp = std::env::temp_dir().join(format!(
        "agy-cli-usage-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let storage = AppStorage::with_base_dir(temp.join("app_data"));
    storage.ensure_dirs().unwrap();

    let today_str = Local::now().format("%Y-%m-%d").to_string();
    let ts_iso = format!("{today_str}T10:15:00+08:00");
    let now_ms = Local::now().timestamp_millis();

    // 1. agy: workspace_sessions.json 归因 + transcript.jsonl
    upsert_workspace_role_session(
        &storage,
        WorkspaceRoleSession {
            workspace: r"D:\work\demo".to_string(),
            workspace_key: String::new(),
            slot_id: "arch-1".to_string(),
            alias: "carbon-arch".to_string(),
            display_name: "总架构师".to_string(),
            role: agy_conductor::orchestrator::types::RoleKind::Architect,
            model_id: "[Gateway] gemini-4-carbon-high".to_string(),
            conversation_id: "conv-agy-100".to_string(),
            last_goal: "audit".to_string(),
            room_id: None,
            updated_at: ts_iso.clone(),
            source: SESSION_SOURCE_CONDUCTOR.to_string(),
            context_tokens: 60_000,
            healthy_for_reuse: true,
            unhealthy_reason: None,
        },
    )
    .unwrap();

    let agy_brain = temp.join("agy_brain");
    let agy_log_dir = agy_brain
        .join("conv-agy-100")
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&agy_log_dir).unwrap();
    let agy_transcript = format!(
        "{}\n",
        json!({
            "step_index": 1,
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": ts_iso,
            "usage": {
                "input_tokens": 50_000,
                "cache_read_tokens": 100_000,
                "output_tokens": 6_000,
                "thinking_tokens": 4_000
            }
        })
    );
    fs::write(agy_log_dir.join("transcript.jsonl"), agy_transcript).unwrap();

    // 2. Codex: 累计 total_token_usage 增量 + input_tokens 含 cached_input_tokens + output 含 reasoning
    let codex_home = temp.join("codex_home");
    let codex_dir = codex_home.join("sessions").join("2026").join("11").join("20");
    fs::create_dir_all(&codex_dir).unwrap();
    let codex_lines = [
        json!({
            "timestamp": ts_iso,
            "type": "turn_context",
            "payload": { "model": "gpt-6.2-astra" }
        }),
        // 首条全 0 快照必须忽略
        json!({
            "timestamp": ts_iso,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 0, "cached_input_tokens": 0,
                        "output_tokens": 0, "reasoning_output_tokens": 0, "total_tokens": 0
                    }
                }
            }
        }),
        // Turn 1: input=100k (内含 cached=80k => non_cached=20k), output=10k (内含 reasoning=4k => 仍只计 10k!)
        json!({
            "timestamp": ts_iso,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 100_000, "cached_input_tokens": 80_000,
                        "output_tokens": 10_000, "reasoning_output_tokens": 4_000, "total_tokens": 110_000
                    }
                }
            }
        }),
        // Turn 2 (累计值): delta input=30k (内含 cached=20k => non_cached=10k), delta output=5k
        json!({
            "timestamp": ts_iso,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 130_000, "cached_input_tokens": 100_000,
                        "output_tokens": 15_000, "reasoning_output_tokens": 6_000, "total_tokens": 145_000
                    }
                }
            }
        }),
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(codex_dir.join("rollout-demo.jsonl"), codex_lines).unwrap();

    // 3. Claude Code: 同 message.id 流式分片去重（仅保留 output_tokens 最大的一条）
    let claude_dir = temp.join("claude_projects").join("demo-proj");
    fs::create_dir_all(&claude_dir).unwrap();
    let claude_lines = [
        json!({
            "timestamp": ts_iso,
            "sessionId": "claude-ses-1",
            "message": {
                "id": "msg_dup_001",
                "model": "claude-opus-5",
                "usage": {
                    "input_tokens": 10_000,
                    "cache_read_input_tokens": 50_000,
                    "cache_creation_input_tokens": 20_000,
                    "output_tokens": 500
                }
            }
        }),
        json!({
            "timestamp": ts_iso,
            "sessionId": "claude-ses-1",
            "message": {
                "id": "msg_dup_001",
                "model": "claude-opus-5",
                "usage": {
                    "input_tokens": 10_000,
                    "cache_read_input_tokens": 50_000,
                    "cache_creation_input_tokens": 20_000,
                    "output_tokens": 4_000
                }
            }
        }),
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(claude_dir.join("ses-1.jsonl"), claude_lines).unwrap();

    // 4. OpenCode: Windows 真实只读 SQLite (含 message + session_message 重复 id 去重)
    let opencode_db = temp.join("opencode").join("opencode.db");
    #[cfg(windows)]
    create_mock_opencode_sqlite_db(&opencode_db, now_ms);

    let roots = UsageScanRoots {
        agy_brain_dir: Some(agy_brain),
        codex_home_dir: Some(codex_home),
        claude_projects_dir: Some(temp.join("claude_projects")),
        opencode_db_paths: vec![opencode_db],
        opencode_legacy_msg_dir: None,
        cache_path: storage.base_dir().join("cli_usage_rollups.json"),
        storage_for_agy_models: Some(storage),
    };
    let pricing = PricingCatalog::builtin_only();

    // 首次冷扫描
    let first = collect_cli_usage_overview_with_roots(&roots, &pricing, UsageRange::Days7, false);
    assert_eq!(first.cache_hits, 0, "first scan should be cold");

    let agy = first.providers.iter().find(|p| p.provider_id == "agy").unwrap();
    assert_eq!(agy.input_tokens, 50_000);
    assert_eq!(agy.cache_read_tokens, 100_000);
    assert_eq!(agy.output_tokens, 10_000); // 6k + 4k thinking
    // Carbon ($2/$10/$0.10): 50k*2 + 100k*0.1 + 10k*10 = 0.21
    assert_close(agy.cost_usd, 0.21, "agy carbon cost");

    let codex = first.providers.iter().find(|p| p.provider_id == "codex").unwrap();
    assert_eq!(codex.input_tokens, 30_000, "codex non-cached input = 130k - 100k");
    assert_eq!(codex.cache_read_tokens, 100_000);
    assert_eq!(codex.output_tokens, 15_000, "codex must NOT double-count reasoning_output_tokens");
    // Astra ($8/$40/$0.40): 30k*8 + 100k*0.4 + 15k*40 = 0.24 + 0.04 + 0.60 = 0.88
    assert_close(codex.cost_usd, 0.88, "codex astra cost");

    let claude = first.providers.iter().find(|p| p.provider_id == "claude").unwrap();
    assert_eq!(claude.steps, 1, "streaming chunks with same message.id must deduplicate to 1");
    assert_eq!(claude.input_tokens, 10_000);
    assert_eq!(claude.cache_read_tokens, 50_000);
    assert_eq!(claude.cache_creation_tokens, 20_000);
    assert_eq!(claude.output_tokens, 4_000);
    // Opus 5 ($5/$25/$0.50, cache_create=$6.25): 10k*5 + 50k*0.5 + 20k*6.25 + 4k*25 = 0.05 + 0.025 + 0.125 + 0.10 = 0.30
    assert_close(claude.cost_usd, 0.30, "claude opus5 cost");

    #[cfg(windows)]
    {
        let oc = first.providers.iter().find(|p| p.provider_id == "opencode").unwrap();
        assert_eq!(oc.steps, 1, "duplicate msg-oc-1 across message & session_message must dedup");
        assert_eq!(oc.input_tokens, 40_000);
        assert_eq!(oc.cache_read_tokens, 100_000);
        assert_eq!(oc.output_tokens, 10_000); // 8k output + 2k reasoning
        // gpt-6-sol ($2/$10/$0.20): 40k*2 + 100k*0.2 + 10k*10 = 0.08 + 0.02 + 0.10 = 0.20
        assert_close(oc.cost_usd, 0.20, "opencode gpt-6-sol cost");
        assert_close(first.total_cost_usd, 0.21 + 0.88 + 0.30 + 0.20, "grand total cost");
    }

    // 二次增量扫描必须 100% 命中磁盘缓存
    let second = collect_cli_usage_overview_with_roots(&roots, &pricing, UsageRange::Days7, false);
    assert_eq!(second.scanned_files, first.scanned_files);
    assert_eq!(
        second.cache_hits, second.scanned_files,
        "second scan must hit rollup cache for every unchanged file"
    );
    assert_close(second.total_cost_usd, first.total_cost_usd, "cached total matches cold total");

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn filters_internal_model_enum_placeholder_in_agy_transcript() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "agy_internal_model_filter_{}_{}",
        std::process::id(),
        nanos
    ));
    let agy_brain = temp.join("brain");
    let ts_iso = Local::now().to_rfc3339();

    // Case A: PLANNER_RESPONSE carries internal enum MODEL_GOOGLE_GEMINI_INTERNAL_BYOM -> fallback to "gemini-4 (agy)"
    let log_a = agy_brain
        .join("conv-internal-fallback")
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&log_a).unwrap();
    let line_a = json!({
        "type": "PLANNER_RESPONSE",
        "created_at": ts_iso,
        "model": "MODEL_GOOGLE_GEMINI_INTERNAL_BYOM",
        "metadata": { "model": "model_internal_placeholder" },
        "usage": {
            "input_tokens": 10_000,
            "cache_read_tokens": 20_000,
            "output_tokens": 2_000,
            "thinking_tokens": 0
        }
    });
    fs::write(log_a.join("transcript.jsonl"), format!("{line_a}\n")).unwrap();

    // Case B: PLANNER_RESPONSE carries MODEL_* but USER_INPUT mentions real model -> infer real model from prompt
    let log_b = agy_brain
        .join("conv-internal-prompt")
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&log_b).unwrap();
    let lines_b = [
        json!({
            "type": "PLANNER_RESPONSE",
            "created_at": ts_iso,
            "model": "MODEL_GOOGLE_GEMINI_INTERNAL_BYOM",
            "usage": { "input_tokens": 1_000, "output_tokens": 500 }
        }),
        json!({
            "type": "USER_INPUT",
            "created_at": ts_iso,
            "content": "当前选用模型：gemini-4-neon-xhigh，请执行分析"
        }),
        json!({
            "type": "PLANNER_RESPONSE",
            "created_at": ts_iso,
            "model": "MODEL_GOOGLE_GEMINI_INTERNAL_BYOM",
            "usage": { "input_tokens": 9_000, "output_tokens": 1_500 }
        }),
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(log_b.join("transcript.jsonl"), lines_b).unwrap();

    // Pre-seed stale v1 cache containing MODEL_* to confirm automatic cache invalidation
    let cache_path = temp.join("cache.json");
    fs::write(
        &cache_path,
        json!({
            "version": 1,
            "files": {
                "stale": {
                    "provider_id": "agy",
                    "mtime_ms": 0,
                    "file_len": 0,
                    "buckets": []
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let roots = UsageScanRoots {
        agy_brain_dir: Some(agy_brain),
        codex_home_dir: None,
        claude_projects_dir: None,
        opencode_db_paths: Vec::new(),
        opencode_legacy_msg_dir: None,
        cache_path,
        storage_for_agy_models: None,
    };
    let pricing = PricingCatalog::builtin_only();
    let overview = collect_cli_usage_overview_with_roots(&roots, &pricing, UsageRange::Days7, false);

    assert_eq!(overview.cache_hits, 0, "v1 stale cache must be invalidated");
    assert!(
        overview
            .models
            .iter()
            .all(|m| !m.model_id.to_ascii_uppercase().starts_with("MODEL_")),
        "internal MODEL_* placeholders must never appear in models list: {:?}",
        overview.models
    );
    assert!(
        overview.models.iter().any(|m| m.model_id == "gemini-4 (agy)"),
        "fallback model label expected: {:?}",
        overview.models
    );
    assert!(
        overview.models.iter().any(|m| m.model_id == "gemini-4-neon-xhigh"),
        "prompt-inferred model expected even after seeing MODEL_* placeholder: {:?}",
        overview.models
    );

    let _ = fs::remove_dir_all(&temp);
}
