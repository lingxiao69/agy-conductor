use crate::ipc::dispatcher::{emit_ui, toast};
use crate::ipc::state::SharedAppState;
use crate::ipc::{AppCustomEvent, UiEvent};
use crate::orchestrator::types::{AgentSlot, SameRoleDispatchMode};
use crate::storage::history::{
    delete_history_room, export_history_markdown, list_history_rooms, load_history_room,
    suggested_markdown_filename,
};
use std::path::PathBuf;
use tao::event_loop::EventLoopProxy;

pub async fn handle_save_roster_preset(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    name: String,
    slots: Vec<AgentSlot>,
    dispatch_mode: SameRoleDispatchMode,
) {
    let outcome = {
        let mut guard = state.lock().await;
        match guard.save_roster_preset(&name, slots, dispatch_mode) {
            Ok((preset, overwritten)) => Ok((preset, overwritten, guard.snapshot())),
            Err(err) => Err(err),
        }
    };

    match outcome {
        Ok((preset, overwritten, snap)) => {
            emit_ui(&proxy, UiEvent::StateSnapshot(snap));
            let msg = if overwritten {
                format!("已覆盖更新编队预设「{}」", preset.name)
            } else {
                format!("已保存编队预设「{}」", preset.name)
            };
            toast(&proxy, "success", msg);
        }
        Err(err) => toast(&proxy, "warning", err),
    }
}

pub async fn handle_apply_roster_preset(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    preset_id: String,
) {
    let outcome = {
        let mut guard = state.lock().await;
        match guard.apply_roster_preset(&preset_id) {
            Ok(preset) => Ok((preset, guard.snapshot())),
            Err(err) => Err(err),
        }
    };

    match outcome {
        Ok((preset, snap)) => {
            emit_ui(&proxy, UiEvent::StateSnapshot(snap));
            toast(&proxy, "success", format!("已切换至编队预设「{}」", preset.name));
        }
        Err(err) => toast(&proxy, "error", err),
    }
}

pub async fn handle_delete_roster_preset(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    preset_id: String,
) {
    let outcome = {
        let mut guard = state.lock().await;
        match guard.delete_roster_preset(&preset_id) {
            Ok(removed) => Ok((removed, guard.snapshot())),
            Err(err) => Err(err),
        }
    };

    match outcome {
        Ok((removed, snap)) => {
            emit_ui(&proxy, UiEvent::StateSnapshot(snap));
            toast(&proxy, "success", format!("已删除编队预设「{}」", removed.name));
        }
        Err(err) => toast(&proxy, "error", err),
    }
}

pub async fn handle_list_history_rooms(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
) {
    let storage = { state.lock().await.storage.clone() };
    let res = tokio::task::spawn_blocking(move || list_history_rooms(&storage)).await;
    match res {
        Ok(Ok(rooms)) => emit_ui(&proxy, UiEvent::HistoryRoomsListed { rooms }),
        Ok(Err(err)) => toast(&proxy, "error", err),
        Err(err) => toast(&proxy, "error", format!("读取历史项目列表失败: {err}")),
    }
}

pub async fn handle_load_history_room(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    room_id: String,
) {
    let storage = { state.lock().await.storage.clone() };
    let res = tokio::task::spawn_blocking(move || load_history_room(&storage, &room_id)).await;
    match res {
        Ok(Ok(detail)) => emit_ui(&proxy, UiEvent::HistoryRoomLoaded { detail }),
        Ok(Err(err)) => toast(&proxy, "error", err),
        Err(err) => toast(&proxy, "error", format!("读取历史项目详情失败: {err}")),
    }
}

pub async fn handle_delete_history_room(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    room_id: String,
) {
    let storage = {
        let guard = state.lock().await;
        if guard.is_room_currently_running(&room_id) {
            toast(
                &proxy,
                "warning",
                "当前历史记录对应正在运行中的活跃会话，请先停止任务后再删除。",
            );
            return;
        }
        guard.storage.clone()
    };

    let room_id_for_io = room_id.clone();
    let res = tokio::task::spawn_blocking(move || {
        delete_history_room(&storage, &room_id_for_io)?;
        list_history_rooms(&storage)
    })
    .await;

    match res {
        Ok(Ok(rooms)) => {
            {
                let mut guard = state.lock().await;
                if !guard.is_run_active()
                    && guard.current_room_id.as_deref() == Some(room_id.as_str())
                {
                    guard.current_room_id = None;
                    guard.current_room_dir = None;
                }
            }
            emit_ui(&proxy, UiEvent::HistoryRoomDeleted { room_id, rooms });
            toast(&proxy, "success", "已删除该条历史项目记录");
        }
        Ok(Err(err)) => toast(&proxy, "error", err),
        Err(err) => toast(&proxy, "error", format!("删除历史记录失败: {err}")),
    }
}

pub async fn handle_export_history_markdown(
    state: SharedAppState,
    proxy: EventLoopProxy<AppCustomEvent>,
    room_id: String,
) {
    let (storage, initial_dir) = {
        let guard = state.lock().await;
        (guard.storage.clone(), guard.config.last_workspace.clone())
    };

    let detail = match tokio::task::spawn_blocking(move || load_history_room(&storage, &room_id))
        .await
    {
        Ok(Ok(detail)) => detail,
        Ok(Err(err)) => {
            toast(&proxy, "error", err);
            return;
        }
        Err(err) => {
            toast(&proxy, "error", format!("读取历史详情失败: {err}"));
            return;
        }
    };

    let default_name = suggested_markdown_filename(&detail);
    let picked = tokio::task::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new()
            .set_title("导出历史项目 Markdown 报告")
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
        // 用户取消文件对话框，不报错
        return;
    };
    if target_path.extension().is_none() {
        target_path.set_extension("md");
    }

    let display_path = target_path.to_string_lossy().to_string();
    let write_res =
        tokio::task::spawn_blocking(move || export_history_markdown(&detail, &target_path)).await;
    match write_res {
        Ok(Ok(())) => toast(&proxy, "success", format!("已导出 Markdown 报告：{display_path}")),
        Ok(Err(err)) => toast(&proxy, "error", err),
        Err(err) => toast(&proxy, "error", format!("导出 Markdown 报告失败: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::ipc::{UiCommand, UiEvent};
    use crate::orchestrator::types::SameRoleDispatchMode;
    use crate::storage::history::{HistoryRoomDetail, HistoryRoomSummary};
    use serde_json::Value;

    #[test]
    fn ui_command_contract_deserializes_all_history_and_preset_variants() {
        let cmds = [
            r#"{"cmd":"save_roster_preset","name":"日常极速组","slots":[],"dispatch_mode":"PriorityWithOverflow"}"#,
            r#"{"cmd":"apply_roster_preset","preset_id":"preset-1"}"#,
            r#"{"cmd":"delete_roster_preset","preset_id":"preset-1"}"#,
            r#"{"cmd":"list_history_rooms"}"#,
            r#"{"cmd":"load_history_room","room_id":"room-1"}"#,
            r#"{"cmd":"delete_history_room","room_id":"room-1"}"#,
            r#"{"cmd":"export_history_markdown","room_id":"room-1"}"#,
        ];

        let parsed: Vec<UiCommand> = cmds
            .iter()
            .map(|raw| serde_json::from_str(raw).expect("valid UiCommand JSON"))
            .collect();

        assert!(matches!(
            &parsed[0],
            UiCommand::SaveRosterPreset {
                name,
                dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
                ..
            } if name == "日常极速组"
        ));
        assert!(matches!(&parsed[1], UiCommand::ApplyRosterPreset { preset_id } if preset_id == "preset-1"));
        assert!(matches!(&parsed[2], UiCommand::DeleteRosterPreset { preset_id } if preset_id == "preset-1"));
        assert!(matches!(&parsed[3], UiCommand::ListHistoryRooms));
        assert!(matches!(&parsed[4], UiCommand::LoadHistoryRoom { room_id } if room_id == "room-1"));
        assert!(matches!(&parsed[5], UiCommand::DeleteHistoryRoom { room_id } if room_id == "room-1"));
        assert!(matches!(&parsed[6], UiCommand::ExportHistoryMarkdown { room_id } if room_id == "room-1"));
    }

    #[test]
    fn ui_event_contract_serializes_exact_tags_and_fields() {
        let summary = HistoryRoomSummary {
            room_id: "room-1".to_string(),
            workspace: "D:/work/demo".to_string(),
            goal: "实现抽屉".to_string(),
            phase: "全部验收通过".to_string(),
            task_count: 3,
            approved_task_count: 3,
            message_count: 10,
            updated_at: "2026-11-20T12:00:00+08:00".to_string(),
        };

        let listed: Value = serde_json::to_value(UiEvent::HistoryRoomsListed {
            rooms: vec![summary.clone()],
        })
        .expect("serialize listed");
        assert_eq!(listed["type"], "history_rooms_listed");
        assert_eq!(listed["rooms"][0]["room_id"], "room-1");
        assert_eq!(listed["rooms"][0]["approved_task_count"], 3);

        let loaded: Value = serde_json::to_value(UiEvent::HistoryRoomLoaded {
            detail: HistoryRoomDetail {
                room_id: "room-1".to_string(),
                workspace: "D:/work/demo".to_string(),
                goal: "实现抽屉".to_string(),
                phase: "全部验收通过".to_string(),
                updated_at: "2026-11-20T12:00:00+08:00".to_string(),
                tasks: Vec::new(),
                messages: Vec::new(),
            },
        })
        .expect("serialize loaded");
        assert_eq!(loaded["type"], "history_room_loaded");
        assert_eq!(loaded["detail"]["room_id"], "room-1");

        let deleted: Value = serde_json::to_value(UiEvent::HistoryRoomDeleted {
            room_id: "room-1".to_string(),
            rooms: Vec::new(),
        })
        .expect("serialize deleted");
        assert_eq!(deleted["type"], "history_room_deleted");
        assert_eq!(deleted["room_id"], "room-1");
        assert!(deleted["rooms"].as_array().unwrap().is_empty());
    }

    #[test]
    fn state_snapshot_event_exposes_presets_and_active_preset_id() {
        let root = std::env::temp_dir().join(format!(
            "agy-conductor-snap-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let storage = crate::storage::store::AppStorage::with_base_dir(root.clone());
        let mut core = crate::ipc::state::AppCoreState::new(storage, None, None);
        let (preset, _) = core
            .save_roster_preset(
                "架构精审组",
                core.config.slots.clone(),
                SameRoleDispatchMode::StrictPrimaryFailover,
            )
            .expect("save preset");
        let val: Value =
            serde_json::to_value(UiEvent::StateSnapshot(core.snapshot())).expect("serialize snap");
        assert_eq!(val["type"], "state_snapshot");
        assert_eq!(val["active_preset_id"], preset.preset_id);
        assert_eq!(val["presets"][0]["name"], "架构精审组");
        assert_eq!(val["presets"][0]["dispatch_mode"], "StrictPrimaryFailover");
        let _ = std::fs::remove_dir_all(root);
    }
}
