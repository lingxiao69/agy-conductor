use crate::agy::models::{fetch_available_models, ModelInfo};
use crate::agy::resolver::{probe_agy_version, ResolvedAgyRuntime};
use crate::orchestrator::types::{AgentSlot, SlotRunState};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct UpdateProgressChunk {
    pub line: String,
    pub finished: bool,
    pub new_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateOutcome {
    pub old_version: String,
    pub new_version: String,
    pub models: Vec<ModelInfo>,
    pub summary: String,
}

/// Pre-update safety guard: prevents replacing `agy.exe` while any background or attached
/// CLI slot holds the binary open on Windows.
pub fn validate_can_update(slots: &[AgentSlot]) -> Result<(), String> {
    let busy: Vec<String> = slots
        .iter()
        .filter(|s| matches!(s.state, SlotRunState::Working | SlotRunState::ExternalCliAttached))
        .map(|s| format!("{} ({:?})", s.display_name, s.state))
        .collect();
    if !busy.is_empty() {
        return Err(format!(
            "当前仍有活跃的 agy 进程占用（{}），请先暂停或停止协同任务后再执行更新。",
            busy.join("、")
        ));
    }
    Ok(())
}

pub async fn run_agy_update(
    runtime: &ResolvedAgyRuntime,
    progress_tx: mpsc::UnboundedSender<UpdateProgressChunk>,
) -> Result<UpdateOutcome, String> {
    let old_version = probe_agy_version(runtime)
        .await
        .unwrap_or_else(|_| "unknown".to_string());

    let _ = progress_tx.send(UpdateProgressChunk {
        line: format!("当前版本: v{old_version}，正在执行 agy update..."),
        finished: false,
        new_version: None,
    });

    if runtime.is_gateway_wrapper() {
        let _ = progress_tx.send(UpdateProgressChunk {
            line: "检测到 Gateway 启动器环境，将仅升级底层 agy.exe 本体并保留 gateway-support 目录。".to_string(),
            finished: false,
            new_version: None,
        });
    }

    let mut cmd = runtime.build_update_command();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法启动 agy update 进程: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let tx_out = progress_tx.clone();
    let out_task = tokio::spawn(async move {
        if let Some(stream) = stdout {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let _ = tx_out.send(UpdateProgressChunk {
                        line: trimmed.to_string(),
                        finished: false,
                        new_version: None,
                    });
                }
            }
        }
    });

    let tx_err = progress_tx.clone();
    let err_task = tokio::spawn(async move {
        let mut last_err = String::new();
        if let Some(stream) = stderr {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    last_err = trimmed.to_string();
                    let _ = tx_err.send(UpdateProgressChunk {
                        line: format!("[stderr] {trimmed}"),
                        finished: false,
                        new_version: None,
                    });
                }
            }
        }
        last_err
    });

    let status = child
        .wait()
        .await
        .map_err(|e| format!("等待 agy update 结束失败: {e}"))?;
    let _ = out_task.await;
    let last_stderr = err_task.await.unwrap_or_default();

    if !status.success() {
        let msg = format!("agy update 执行失败 ({status}) {}", last_stderr);
        let _ = progress_tx.send(UpdateProgressChunk {
            line: msg.clone(),
            finished: true,
            new_version: None,
        });
        return Err(msg);
    }

    let _ = progress_tx.send(UpdateProgressChunk {
        line: "更新完成，正在重新检测版本并同步本地/网关模型列表...".to_string(),
        finished: false,
        new_version: None,
    });

    let new_version = probe_agy_version(runtime)
        .await
        .unwrap_or_else(|_| old_version.clone());
    let models = fetch_available_models(runtime).await.unwrap_or_default();

    let summary = format!(
        "agy 已从 v{old_version} 更新至 v{new_version}，共发现 {} 个可用模型。",
        models.len()
    );
    let _ = progress_tx.send(UpdateProgressChunk {
        line: summary.clone(),
        finished: true,
        new_version: Some(new_version.clone()),
    });

    Ok(UpdateOutcome {
        old_version,
        new_version,
        models,
        summary,
    })
}
