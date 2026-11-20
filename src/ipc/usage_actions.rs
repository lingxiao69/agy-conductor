use crate::ipc::dispatcher::{emit_ui, toast};
use crate::ipc::state::SharedAppState;
use crate::ipc::{AppCustomEvent, UiEvent};
use crate::storage::store::atomic_write_text;
use crate::usage_stats::{collect_cli_usage_overview, UsageRange};
use chrono::Local;
use std::path::PathBuf;
use tao::event_loop::EventLoopProxy;

pub async fn handle_load_cli_usage_stats(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    range: UsageRange,
    force_refresh: bool,
) {
    let storage = { state.lock().await.storage.clone() };
    let res = tokio::task::spawn_blocking(move || {
        collect_cli_usage_overview(&storage, range, force_refresh)
    })
    .await;

    match res {
        Ok(overview) => {
            emit_ui(&proxy, UiEvent::CliUsageStatsLoaded { overview });
            if force_refresh {
                toast(&proxy, "success", "已重新同步本机四大 CLI 用量记录");
            }
        }
        Err(e) => toast(&proxy, "error", format!("聚合本地 CLI 用量失败: {e}")),
    }
}

pub async fn handle_export_report_markdown(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    title: String,
    markdown: String,
) {
    let initial_dir = { state.lock().await.config.last_workspace.clone() };
    let safe_title: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') || !c.is_ascii() {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect::<String>()
        .trim()
        .to_string();
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let default_name = if safe_title.is_empty() {
        format!("AgyConductor-报告-{stamp}.md")
    } else {
        format!("{safe_title}-{stamp}.md")
    };

    let picked = tokio::task::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new()
            .set_title("导出报告为 Markdown 文档")
            .add_filter("Markdown 文档", &["md"])
            .set_file_name(&default_name);
        if !initial_dir.is_empty() && PathBuf::from(&initial_dir).is_dir() {
            dialog = dialog.set_directory(&initial_dir);
        }
        dialog.save_file()
    })
    .await
    .ok()
    .flatten();

    let Some(mut target_path) = picked else {
        return;
    };
    if target_path.extension().is_none() {
        target_path.set_extension("md");
    }

    let display_path = target_path.to_string_lossy().to_string();
    let write_res = tokio::task::spawn_blocking(move || atomic_write_text(&target_path, &markdown)).await;
    match write_res {
        Ok(Ok(())) => toast(&proxy, "success", format!("已导出 Markdown 报告：{display_path}")),
        Ok(Err(e)) => toast(&proxy, "error", e),
        Err(e) => toast(&proxy, "error", format!("导出 Markdown 报告失败: {e}")),
    }
}
