use crate::ipc::dispatcher::dispatch_ui_command;
use crate::ipc::state::AppCoreState;
use crate::ipc::AppCustomEvent;
use crate::storage::store::AppStorage;
use crate::win_guard::job_object::create_shared_job_guard;
use std::sync::Arc;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use tokio::sync::Mutex;
use wry::http::header::CONTENT_TYPE;
use wry::http::{Request, Response};
use wry::WebViewBuilder;

const ASSET_INDEX_HTML: &str = include_str!("../assets/index.html");
const ASSET_TOKENS_CSS: &str = include_str!("../assets/styles/tokens.css");
const ASSET_LAYOUT_CSS: &str = include_str!("../assets/styles/layout.css");
const ASSET_CONTROLS_CSS: &str = include_str!("../assets/styles/controls.css");
const ASSET_COMPONENTS_CSS: &str = include_str!("../assets/styles/components.css");
const ASSET_CHAT_MODALS_CSS: &str = include_str!("../assets/styles/chat_and_modals.css");
const ASSET_HISTORY_PRESETS_CSS: &str = include_str!("../assets/styles/history_and_presets.css");
const ASSET_TERMINAL_SESSIONS_CSS: &str =
    include_str!("../assets/styles/terminal_and_sessions.css");
const ASSET_CLI_RAIL_BILLING_CSS: &str =
    include_str!("../assets/styles/cli_rail_and_billing.css");
const ASSET_REPORT_MODAL_CSS: &str = include_str!("../assets/styles/report_modal.css");
const ASSET_MODALS_ANALYTICS_CSS: &str =
    include_str!("../assets/styles/modals_and_analytics.css");
const ASSET_LIVE_SIDE_PANE_CSS: &str = include_str!("../assets/styles/live_side_pane.css");
const ASSET_MAIN_JS: &str = include_str!("../assets/js/main.js");
const ASSET_ROSTER_JS: &str = include_str!("../assets/js/roster_view.js");
const ASSET_CHAT_JS: &str = include_str!("../assets/js/chat_view.js");
const ASSET_TASK_MODAL_JS: &str = include_str!("../assets/js/task_and_modal.js");
const ASSET_PRESETS_JS: &str = include_str!("../assets/js/presets_view.js");
const ASSET_HISTORY_JS: &str = include_str!("../assets/js/history_drawer.js");
const ASSET_TERMINAL_DRAWER_JS: &str = include_str!("../assets/js/terminal_drawer.js");
const ASSET_SESSION_RESUME_JS: &str = include_str!("../assets/js/session_resume_view.js");
const ASSET_CLI_RAIL_JS: &str = include_str!("../assets/js/cli_rail_view.js");
const ASSET_BILLING_VIEW_JS: &str = include_str!("../assets/js/billing_view.js");
const ASSET_REPORT_MODAL_JS: &str = include_str!("../assets/js/report_modal.js");
const ASSET_USAGE_CHARTS_JS: &str = include_str!("../assets/js/usage_charts.js");
const ASSET_USAGE_ANALYTICS_JS: &str = include_str!("../assets/js/usage_analytics_view.js");
const ASSET_LIVE_SIDE_PANE_JS: &str = include_str!("../assets/js/live_side_pane.js");

pub fn run_gui_app() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()
        .map_err(|e| format!("初始化 Tokio 异步运行时失败: {e}"))?;

    let event_loop = EventLoopBuilder::<AppCustomEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("轻舟 AgyConductor — 多模型自动研发协同台")
        .with_inner_size(LogicalSize::new(1220.0, 780.0))
        .with_min_inner_size(LogicalSize::new(960.0, 640.0))
        .build(&event_loop)
        .map_err(|e| format!("创建桌面主窗口失败: {e}"))?;

    #[cfg(windows)]
    let main_hwnd: Option<isize> = {
        use tao::platform::windows::WindowExtWindows;
        Some(window.hwnd())
    };
    #[cfg(not(windows))]
    let main_hwnd: Option<isize> = None;

    let job_guard = create_shared_job_guard();
    let storage = AppStorage::default_location();
    let _ = storage.ensure_dirs();
    let shared_state = Arc::new(Mutex::new(AppCoreState::new(
        storage,
        job_guard.clone(),
        main_hwnd,
    )));

    let ipc_proxy = proxy.clone();
    let builder = WebViewBuilder::new()
        .with_custom_protocol("agyhub".into(), |_webview_id, request| {
            serve_embedded_asset(request).map(Into::into)
        })
        .with_ipc_handler(move |req| {
            let body = req.body().to_string();
            let _ = ipc_proxy.send_event(AppCustomEvent::UiCommandReceived(body));
        })
        .with_url("agyhub://app/index.html");

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let webview = builder
        .build(&window)
        .map_err(|e| format!("初始化 Edge WebView2 视图失败: {e}"))?;

    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| "无法获取 GTK 默认容器".to_string())?;
        builder
            .build_gtk(vbox)
            .map_err(|e| format!("初始化 WebView 失败: {e}"))?
    };

    let mut webview_opt = Some(webview);
    let rt_handle = runtime.handle().clone();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let stop_sent = rt_handle.block_on(async {
                    if let Ok(mut guard) = tokio::time::timeout(
                        std::time::Duration::from_millis(250),
                        shared_state.lock(),
                    )
                    .await
                    {
                        if let Some(tx) = guard.active_engine_tx.take() {
                            let _ = tx.send(crate::orchestrator::engine::EngineControl::Stop);
                            return true;
                        }
                    }
                    false
                });
                if stop_sent {
                    std::thread::sleep(std::time::Duration::from_millis(160));
                }
                if let Some(guard) = &job_guard {
                    guard.terminate_all();
                }
                let _ = webview_opt.take();
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(custom) => match custom {
                AppCustomEvent::UiCommandReceived(raw_json) => {
                    let st = Arc::clone(&shared_state);
                    let prx = proxy.clone();
                    rt_handle.spawn(async move {
                        dispatch_ui_command(raw_json, st, prx).await;
                    });
                }
                AppCustomEvent::DispatchUiEvent(ui_event) => {
                    if let Some(wv) = webview_opt.as_ref() {
                        if let Ok(json_payload) = serde_json::to_string(&ui_event) {
                            let script = format!(
                                "if (window.__agyDispatch) {{ window.__agyDispatch({json_payload}); }}"
                            );
                            let _ = wv.evaluate_script(&script);
                        }
                    }
                }
            },
            _ => {}
        }
    });
}

fn serve_embedded_asset(request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let raw_path = request.uri().path();
    let trimmed = raw_path.trim_start_matches('/');
    let normalized = trimmed.strip_prefix("app/").unwrap_or(trimmed);

    let (mime, body_str): (&str, Option<&str>) = match normalized {
        "" | "index.html" => ("text/html; charset=utf-8", Some(ASSET_INDEX_HTML)),
        "styles/tokens.css" => ("text/css; charset=utf-8", Some(ASSET_TOKENS_CSS)),
        "styles/layout.css" => ("text/css; charset=utf-8", Some(ASSET_LAYOUT_CSS)),
        "styles/controls.css" => ("text/css; charset=utf-8", Some(ASSET_CONTROLS_CSS)),
        "styles/components.css" => ("text/css; charset=utf-8", Some(ASSET_COMPONENTS_CSS)),
        "styles/chat_and_modals.css" => ("text/css; charset=utf-8", Some(ASSET_CHAT_MODALS_CSS)),
        "styles/history_and_presets.css" => {
            ("text/css; charset=utf-8", Some(ASSET_HISTORY_PRESETS_CSS))
        }
        "styles/terminal_and_sessions.css" => {
            ("text/css; charset=utf-8", Some(ASSET_TERMINAL_SESSIONS_CSS))
        }
        "styles/cli_rail_and_billing.css" => {
            ("text/css; charset=utf-8", Some(ASSET_CLI_RAIL_BILLING_CSS))
        }
        "styles/report_modal.css" => ("text/css; charset=utf-8", Some(ASSET_REPORT_MODAL_CSS)),
        "styles/modals_and_analytics.css" => {
            ("text/css; charset=utf-8", Some(ASSET_MODALS_ANALYTICS_CSS))
        }
        "styles/live_side_pane.css" => ("text/css; charset=utf-8", Some(ASSET_LIVE_SIDE_PANE_CSS)),
        "js/main.js" => ("text/javascript; charset=utf-8", Some(ASSET_MAIN_JS)),
        "js/roster_view.js" => ("text/javascript; charset=utf-8", Some(ASSET_ROSTER_JS)),
        "js/chat_view.js" => ("text/javascript; charset=utf-8", Some(ASSET_CHAT_JS)),
        "js/task_and_modal.js" => ("text/javascript; charset=utf-8", Some(ASSET_TASK_MODAL_JS)),
        "js/presets_view.js" => ("text/javascript; charset=utf-8", Some(ASSET_PRESETS_JS)),
        "js/history_drawer.js" => ("text/javascript; charset=utf-8", Some(ASSET_HISTORY_JS)),
        "js/terminal_drawer.js" => ("text/javascript; charset=utf-8", Some(ASSET_TERMINAL_DRAWER_JS)),
        "js/session_resume_view.js" => {
            ("text/javascript; charset=utf-8", Some(ASSET_SESSION_RESUME_JS))
        }
        "js/cli_rail_view.js" => ("text/javascript; charset=utf-8", Some(ASSET_CLI_RAIL_JS)),
        "js/billing_view.js" => ("text/javascript; charset=utf-8", Some(ASSET_BILLING_VIEW_JS)),
        "js/report_modal.js" => ("text/javascript; charset=utf-8", Some(ASSET_REPORT_MODAL_JS)),
        "js/usage_charts.js" => ("text/javascript; charset=utf-8", Some(ASSET_USAGE_CHARTS_JS)),
        "js/usage_analytics_view.js" => {
            ("text/javascript; charset=utf-8", Some(ASSET_USAGE_ANALYTICS_JS))
        }
        "js/live_side_pane.js" => ("text/javascript; charset=utf-8", Some(ASSET_LIVE_SIDE_PANE_JS)),
        _ => ("text/plain; charset=utf-8", None),
    };

    match body_str {
        Some(content) => Response::builder()
            .status(200)
            .header(CONTENT_TYPE, mime)
            .header("Cache-Control", "no-store")
            .body(content.as_bytes().to_vec())
            .unwrap_or_else(|_| Response::new(Vec::new())),
        None => Response::builder()
            .status(404)
            .header(CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(format!("Asset not found: {raw_path}").into_bytes())
            .unwrap_or_else(|_| Response::new(Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_asset(uri: &str) -> Response<Vec<u8>> {
        let req = Request::builder()
            .uri(uri)
            .body(Vec::new())
            .expect("build asset request");
        serve_embedded_asset(req)
    }

    #[test]
    fn embedded_assets_serve_index_and_new_history_preset_files() {
        let cases = [
            ("agyhub://app/", "text/html; charset=utf-8"),
            ("agyhub://app/index.html", "text/html; charset=utf-8"),
            ("agyhub://localhost/app/index.html", "text/html; charset=utf-8"),
            ("agyhub://app/styles/tokens.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/layout.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/controls.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/components.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/chat_and_modals.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/history_and_presets.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/terminal_and_sessions.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/cli_rail_and_billing.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/report_modal.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/modals_and_analytics.css", "text/css; charset=utf-8"),
            ("agyhub://app/styles/live_side_pane.css", "text/css; charset=utf-8"),
            ("agyhub://app/js/main.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/roster_view.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/chat_view.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/task_and_modal.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/presets_view.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/history_drawer.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/terminal_drawer.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/session_resume_view.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/cli_rail_view.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/billing_view.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/report_modal.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/usage_charts.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/usage_analytics_view.js", "text/javascript; charset=utf-8"),
            ("agyhub://app/js/live_side_pane.js", "text/javascript; charset=utf-8"),
        ];

        for (uri, expected_mime) in cases {
            let resp = request_asset(uri);
            assert_eq!(resp.status(), 200, "expected 200 for {uri}");
            let mime = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            assert_eq!(mime, expected_mime, "unexpected Content-Type for {uri}");
            assert!(!resp.body().is_empty(), "expected non-empty body for {uri}");
        }

        assert!(ASSET_INDEX_HTML.contains("./styles/history_and_presets.css"));
        assert!(ASSET_INDEX_HTML.contains("./styles/terminal_and_sessions.css"));
        assert!(ASSET_INDEX_HTML.contains("./styles/cli_rail_and_billing.css"));
        assert!(ASSET_INDEX_HTML.contains("./styles/report_modal.css"));
        assert!(ASSET_INDEX_HTML.contains("./styles/modals_and_analytics.css"));
        assert!(ASSET_INDEX_HTML.contains("./styles/live_side_pane.css"));
        assert!(ASSET_MAIN_JS.contains("./presets_view.js"));
        assert!(ASSET_MAIN_JS.contains("./history_drawer.js"));
        assert!(ASSET_MAIN_JS.contains("./terminal_drawer.js"));
        assert!(ASSET_MAIN_JS.contains("./session_resume_view.js"));
        assert!(ASSET_MAIN_JS.contains("./cli_rail_view.js"));
        assert!(ASSET_MAIN_JS.contains("./billing_view.js"));
        assert!(ASSET_MAIN_JS.contains("./report_modal.js"));
        assert!(ASSET_MAIN_JS.contains("./usage_analytics_view.js"));
        assert!(ASSET_MAIN_JS.contains("./live_side_pane.js"));
        assert!(ASSET_HISTORY_JS.contains("./chat_view.js"));
    }

    #[test]
    fn embedded_assets_returns_404_for_unknown_path() {
        let resp = request_asset("agyhub://app/js/does_not_exist.js");
        assert_eq!(resp.status(), 404);
    }
}
