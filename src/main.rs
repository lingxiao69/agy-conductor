#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use agy_conductor::agy::models::{build_default_roster, fetch_available_models};
use agy_conductor::agy::resolver::{
    detect_user_prefers_gateway, probe_agy_version, resolve_agy_runtime,
};
use agy_conductor::app_window::run_gui_app;
use agy_conductor::mcp_bridge::stdio_server::{run_mcp_stdio_server, McpBridgeConfig};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if let Some(first) = args.first() {
        if first == "mcp-stdio" {
            let cfg = parse_mcp_args(&args[1..]);
            if let Err(err) = run_mcp_stdio_server(cfg) {
                eprintln!("[agy-conductor mcp-stdio] error: {err}");
                std::process::exit(1);
            }
            return;
        }
        if first == "--probe" || first == "probe" {
            let code = run_smoke_probe();
            std::process::exit(code);
        }
    }

    if let Err(err) = run_gui_app() {
        eprintln!("[agy-conductor] startup error: {err}");
        std::process::exit(1);
    }
}

fn parse_mcp_args(args: &[String]) -> McpBridgeConfig {
    let mut room_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut default_alias = "external-cli".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--room" if i + 1 < args.len() => {
                room_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--alias" if i + 1 < args.len() => {
                default_alias = args[i + 1].clone();
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    McpBridgeConfig {
        room_dir,
        default_alias,
    }
}

fn run_smoke_probe() -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Tokio init error: {e}");
            return 1;
        }
    };

    rt.block_on(async {
        let resolved = match resolve_agy_runtime(None) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Resolve agy failed: {e}");
                return 1;
            }
        };
        let version = match probe_agy_version(&resolved).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Probe version failed: {e}");
                return 1;
            }
        };
        let models = match fetch_available_models(&resolved).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Probe models failed: {e}");
                return 1;
            }
        };
        let prefer_gw = detect_user_prefers_gateway();
        let roster = build_default_roster(&models, prefer_gw);

        let payload = serde_json::json!({
            "runtime_kind": resolved.runtime_kind_label(),
            "runtime_path": resolved.display_path.to_string_lossy(),
            "version": version,
            "prefer_gateway": prefer_gw,
            "total_models": models.len(),
            "gateway_models": models.iter().filter(|m| m.is_gateway).count(),
            "official_models": models.iter().filter(|m| !m.is_gateway).count(),
            "families": {
                "Carbon": models.iter().filter(|m| m.family == agy_conductor::agy::models::ModelFamily::Carbon).count(),
                "Neon": models.iter().filter(|m| m.family == agy_conductor::agy::models::ModelFamily::Neon).count(),
                "Hydrogen": models.iter().filter(|m| m.family == agy_conductor::agy::models::ModelFamily::Hydrogen).count(),
                "Argon": models.iter().filter(|m| m.family == agy_conductor::agy::models::ModelFamily::Argon).count(),
            },
            "default_roster": roster.iter().map(|s| serde_json::json!({
                "slot_id": s.slot_id,
                "role": s.role,
                "priority": s.priority,
                "model_id": s.model_id
            })).collect::<Vec<_>>()
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
        );
        0
    })
}
