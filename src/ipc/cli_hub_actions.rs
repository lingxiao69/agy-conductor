use crate::cli_hub::{
    default_cli_providers, detect_all_cli_providers, launch_provider_external_cli, CliProviderId,
    CliProviderStatus, CliSupportTier,
};
use crate::ipc::dispatcher::{emit_ui, toast};
use crate::ipc::state::SharedAppState;
use crate::ipc::{AppCustomEvent, UiEvent};
use std::path::PathBuf;
use tao::event_loop::EventLoopProxy;

pub async fn handle_select_cli_provider(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    provider: CliProviderId,
) {
    let providers = {
        let mut guard = state.lock().await;
        guard.config.active_cli_provider = provider;
        guard.save_persisted_config();
        guard.cli_providers.clone()
    };

    emit_ui(
        &proxy,
        UiEvent::CliProvidersUpdated {
            active_provider: provider,
            providers,
        },
    );
}

pub async fn handle_refresh_cli_providers(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    notify_toast: bool,
) {
    let (runtime, agy_version) = {
        let guard = state.lock().await;
        (guard.runtime.clone(), guard.agy_version.clone())
    };

    let detected = detect_all_cli_providers(runtime.as_ref(), Some(&agy_version)).await;
    let active_provider = {
        let mut guard = state.lock().await;
        guard.cli_providers = detected.clone();
        guard.config.active_cli_provider
    };

    let installed_count = detected.iter().filter(|p| p.installed).count();
    emit_ui(
        &proxy,
        UiEvent::CliProvidersUpdated {
            active_provider,
            providers: detected,
        },
    );

    if notify_toast {
        toast(
            &proxy,
            "success",
            format!("已重新扫描本机 AI CLI 环境：检测到 {installed_count} / 4 个可用引擎"),
        );
    }
}

pub async fn handle_launch_provider_external_cli(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    provider: CliProviderId,
) {
    let (status, workspace, runtime, room_dir) = {
        let guard = state.lock().await;
        let status = guard
            .cli_providers
            .iter()
            .find(|p| p.id == provider)
            .cloned()
            .unwrap_or_else(|| {
                default_cli_providers()
                    .into_iter()
                    .find(|p| p.id == provider)
                    .expect("provider in default list")
            });
        (
            status,
            PathBuf::from(guard.config.last_workspace.trim()),
            guard.runtime.clone(),
            guard.current_room_dir.clone(),
        )
    };

    let result = tokio::task::spawn_blocking(move || {
        launch_provider_external_cli(
            provider,
            &status,
            &workspace,
            runtime.as_ref(),
            room_dir.as_deref(),
        )
    })
    .await;

    match result {
        Ok(Ok(msg)) => toast(&proxy, "success", msg),
        Ok(Err(err)) => toast(&proxy, "warning", err),
        Err(e) => toast(&proxy, "error", format!("拉起外部 CLI 终端失败: {e}")),
    }
}

/// 在 agy 刷新或更新完成后同步左侧 CLI 导航坞中的 agy 版本与路径状态。
pub async fn sync_agy_provider_status(
    state: &SharedAppState,
    proxy: &EventLoopProxy<AppCustomEvent>,
) {
    let (active_provider, providers) = {
        let mut guard = state.lock().await;
        let version = guard.agy_version.clone();
        let path = guard.runtime_path.clone();
        let installed = guard.runtime.is_some();
        if let Some(agy_entry) = guard
            .cli_providers
            .iter_mut()
            .find(|p| p.id == CliProviderId::Agy)
        {
            agy_entry.installed = installed;
            agy_entry.executable_path = if path.is_empty() { None } else { Some(path) };
            agy_entry.version_hint = if version.is_empty() || version == "检测中..." {
                None
            } else {
                Some(version)
            };
            agy_entry.support_tier = CliSupportTier::ActiveOrchestrator;
        } else {
            guard.cli_providers.insert(
                0,
                CliProviderStatus {
                    id: CliProviderId::Agy,
                    display_name: CliProviderId::Agy.display_name().to_string(),
                    short_label: CliProviderId::Agy.short_label().to_string(),
                    command_name: CliProviderId::Agy.command_name().to_string(),
                    installed,
                    executable_path: if path.is_empty() { None } else { Some(path) },
                    version_hint: Some(version),
                    support_tier: CliSupportTier::ActiveOrchestrator,
                    tagline: CliProviderId::Agy.tagline().to_string(),
                },
            );
        }
        (guard.config.active_cli_provider, guard.cli_providers.clone())
    };

    emit_ui(
        proxy,
        UiEvent::CliProvidersUpdated {
            active_provider,
            providers,
        },
    );
}
