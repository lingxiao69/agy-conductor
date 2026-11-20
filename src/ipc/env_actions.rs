use crate::agy::gateway_wildcards::sanitize_wildcard_model_id;
use crate::agy::models::{build_default_roster, fetch_available_models};
use crate::agy::resolver::{probe_agy_version, resolve_agy_runtime};
use crate::agy::updater::{run_agy_update, validate_can_update};
use crate::billing::SlotUsageStats;
use crate::ipc::cli_hub_actions::sync_agy_provider_status;
use crate::ipc::dispatcher::{emit_ui, toast};
use crate::ipc::state::SharedAppState;
use crate::ipc::{AppCustomEvent, UiEvent};
use tao::event_loop::EventLoopProxy;
use tokio::sync::mpsc;

pub async fn refresh_agy_environment(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    notify_toast: bool,
) {
    let custom_path = { state.lock().await.config.custom_agy_path.clone() };
    let resolved = match resolve_agy_runtime(custom_path.as_deref()) {
        Ok(rt) => rt,
        Err(err) => {
            toast(&proxy, "error", err);
            return;
        }
    };

    let version = probe_agy_version(&resolved)
        .await
        .unwrap_or_else(|_| "unknown".to_string());
    let models = match fetch_available_models(&resolved).await {
        Ok(list) => list,
        Err(e) => {
            toast(&proxy, "error", e);
            Vec::new()
        }
    };

    let (prefer_gateway, slots, runtime_kind, runtime_path) = {
        let mut guard = state.lock().await;
        let kind = resolved.runtime_kind_label();
        let path = resolved.display_path.to_string_lossy().to_string();
        guard.runtime = Some(resolved);
        guard.agy_version = version.clone();
        guard.runtime_kind = kind.clone();
        guard.runtime_path = path.clone();
        guard.models = models.clone();

        let mut repaired = false;
        for slot in &mut guard.config.slots {
            let cleaned = sanitize_wildcard_model_id(&slot.model_id);
            if cleaned != slot.model_id {
                slot.model_id = cleaned;
                slot.usage = SlotUsageStats::for_model(&slot.model_id);
                repaired = true;
            }
        }
        for preset in &mut guard.config.presets {
            for slot in &mut preset.slots {
                let cleaned = sanitize_wildcard_model_id(&slot.model_id);
                if cleaned != slot.model_id {
                    slot.model_id = cleaned;
                    slot.usage = SlotUsageStats::for_model(&slot.model_id);
                    repaired = true;
                }
            }
        }

        if !models.is_empty() {
            let valid_ids: std::collections::HashSet<&str> =
                models.iter().map(|m| m.id.as_str()).collect();
            let needs_rebind = guard
                .config
                .slots
                .iter()
                .all(|s| !valid_ids.contains(s.model_id.as_str()));
            if guard.config.slots.is_empty() || needs_rebind {
                guard.config.slots =
                    build_default_roster(&models, guard.config.prefer_gateway_models);
                guard.config.active_preset_id = None;
                repaired = true;
            }
        }
        if repaired {
            guard.save_persisted_config();
        }
        (
            guard.config.prefer_gateway_models,
            guard.config.slots.clone(),
            kind,
            path,
        )
    };

    emit_ui(
        &proxy,
        UiEvent::ModelsUpdated {
            version: version.clone(),
            runtime_kind,
            runtime_path,
            models: models.clone(),
            prefer_gateway,
            slots,
        },
    );
    sync_agy_provider_status(&state, &proxy).await;

    if notify_toast {
        toast(
            &proxy,
            "success",
            format!("已同步 agy v{version}，共探测到 {} 个本地/网关模型", models.len()),
        );
    }
}

pub async fn handle_run_agy_update(state: SharedAppState, proxy: EventLoopProxy<AppCustomEvent>) {
    let runtime = {
        let mut guard = state.lock().await;
        if guard.update_in_progress {
            toast(&proxy, "info", "agy 更新正在进行中，请稍候...");
            return;
        }
        if let Err(reason) = validate_can_update(&guard.config.slots) {
            emit_ui(
                &proxy,
                UiEvent::UpdateLogChunk {
                    line: reason.clone(),
                    finished: true,
                    new_version: None,
                },
            );
            toast(&proxy, "warning", reason);
            return;
        }
        let Some(rt) = guard.runtime.clone() else {
            toast(&proxy, "error", "未检测到 agy 安装路径。");
            return;
        };
        guard.update_in_progress = true;
        rt
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<crate::agy::updater::UpdateProgressChunk>();
    let proxy_stream = proxy.clone();
    tokio::spawn(async move {
        while let Some(chunk) = rx.recv().await {
            emit_ui(
                &proxy_stream,
                UiEvent::UpdateLogChunk {
                    line: chunk.line,
                    finished: chunk.finished,
                    new_version: chunk.new_version,
                },
            );
        }
    });

    let outcome = run_agy_update(&runtime, tx).await;
    match outcome {
        Ok(res) => {
            let (runtime_kind, runtime_path, prefer_gateway, slots) = {
                let mut guard = state.lock().await;
                guard.update_in_progress = false;
                guard.agy_version = res.new_version.clone();
                guard.models = res.models.clone();
                (
                    guard.runtime_kind.clone(),
                    guard.runtime_path.clone(),
                    guard.config.prefer_gateway_models,
                    guard.config.slots.clone(),
                )
            };
            emit_ui(
                &proxy,
                UiEvent::ModelsUpdated {
                    version: res.new_version,
                    runtime_kind,
                    runtime_path,
                    models: res.models,
                    prefer_gateway,
                    slots,
                },
            );
            sync_agy_provider_status(&state, &proxy).await;
            toast(&proxy, "success", res.summary);
        }
        Err(err) => {
            state.lock().await.update_in_progress = false;
            toast(&proxy, "error", err);
        }
    }
}
