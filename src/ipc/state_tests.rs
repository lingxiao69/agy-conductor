use super::*;
use crate::orchestrator::types::RoleKind;
use std::fs;

struct TempStateEnv {
    root: PathBuf,
    state: AppCoreState,
}

impl TempStateEnv {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "agy-conductor-state-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let storage = AppStorage::with_base_dir(root.clone());
        storage.ensure_dirs().expect("ensure temp dirs");
        let state = AppCoreState::new(storage, None, None);
        Self { root, state }
    }
}

impl Drop for TempStateEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn mk_slot(id: &str, role: RoleKind, model: &str, priority: u8) -> AgentSlot {
    AgentSlot {
        slot_id: id.to_string(),
        alias: id.to_string(),
        display_name: format!("Slot-{id}"),
        model_id: model.to_string(),
        role,
        priority,
        enabled: true,
        state: SlotRunState::Idle,
        conversation_id: None,
        session_reused: false,
        current_activity: None,
        consecutive_failures: 0,
        usage: SlotUsageStats::default(),
    }
}

#[test]
fn apply_preset_idle_clears_old_conversations_and_manual_edit_clears_active_id() {
    let mut env = TempStateEnv::new("idle-switch");
    let mut dirty = mk_slot("coder-1", RoleKind::Coder, "neon-fast", 1);
    dirty.conversation_id = Some("old-conv-id".to_string());
    dirty.state = SlotRunState::Working;
    env.state.config.slots = vec![dirty];
    env.state.phase = RunPhase::Idle;

    let (preset, _) = env
        .state
        .save_roster_preset(
            "日常极速组",
            vec![mk_slot("coder-1", RoleKind::Coder, "neon-fast", 2)],
            SameRoleDispatchMode::StrictPrimaryFailover,
        )
        .expect("save preset");
    assert_eq!(env.state.snapshot().presets.len(), 1);
    assert_eq!(
        env.state.snapshot().active_preset_id.as_deref(),
        Some(preset.preset_id.as_str())
    );

    env.state
        .apply_roster_preset(&preset.preset_id)
        .expect("apply preset in idle");
    assert_eq!(
        env.state.config.dispatch_mode,
        SameRoleDispatchMode::StrictPrimaryFailover
    );
    assert_eq!(env.state.config.slots[0].priority, 2);
    assert_eq!(env.state.config.slots[0].state, SlotRunState::Idle);
    assert!(env.state.config.slots[0].conversation_id.is_none());

    env.state.set_slots_and_mode(
        vec![mk_slot("coder-1", RoleKind::Coder, "neon-pro", 1)],
        SameRoleDispatchMode::PriorityWithOverflow,
    );
    assert!(env.state.config.active_preset_id.is_none());
    assert!(env.state.storage.load_config().active_preset_id.is_none());

    env.state
        .apply_roster_preset(&preset.preset_id)
        .expect("re-apply preset");
    assert!(env.state.config.active_preset_id.is_some());
    env.state.apply_default_roster();
    assert!(env.state.config.active_preset_id.is_none());
    assert!(env.state.storage.load_config().active_preset_id.is_none());
}

#[test]
fn apply_preset_while_running_preserves_matching_runtime_and_notifies_engine() {
    let mut env = TempStateEnv::new("running-switch");
    let (tx, mut rx) = mpsc::unbounded_channel();
    env.state.active_engine_tx = Some(tx);
    env.state.phase = RunPhase::Running;
    env.state.current_room_id = Some("room-live-1".to_string());

    let mut keep = mk_slot("coder-keep", RoleKind::Coder, "neon-xhigh", 1);
    keep.state = SlotRunState::Working;
    keep.conversation_id = Some("conv-keep".to_string());
    keep.current_activity = Some("编码 T1".to_string());
    keep.consecutive_failures = 1;

    let mut swap_model = mk_slot("coder-swap", RoleKind::Coder, "neon-old", 2);
    swap_model.state = SlotRunState::Working;
    swap_model.conversation_id = Some("conv-old".to_string());

    let mut swap_role = mk_slot("slot-role", RoleKind::Coder, "carbon-pro", 3);
    swap_role.state = SlotRunState::QuotaCooling;
    swap_role.conversation_id = Some("conv-role".to_string());

    env.state.config.slots = vec![keep, swap_model, swap_role];
    let (preset, _) = env
        .state
        .save_roster_preset(
            "架构精审组",
            vec![
                mk_slot("coder-keep", RoleKind::Coder, "neon-xhigh", 4),
                mk_slot("coder-swap", RoleKind::Coder, "neon-new", 2),
                mk_slot("slot-role", RoleKind::Reviewer, "carbon-pro", 1),
            ],
            SameRoleDispatchMode::StrictPrimaryFailover,
        )
        .expect("save preset");

    env.state
        .apply_roster_preset(&preset.preset_id)
        .expect("apply while running");
    let applied = &env.state.config.slots;
    assert_eq!(applied[0].priority, 4);
    assert_eq!(applied[0].state, SlotRunState::Working);
    assert_eq!(applied[0].conversation_id.as_deref(), Some("conv-keep"));
    assert_eq!(applied[0].consecutive_failures, 1);

    assert_eq!(applied[1].state, SlotRunState::Idle);
    assert!(applied[1].conversation_id.is_none());
    assert_eq!(applied[2].state, SlotRunState::Idle);
    assert!(applied[2].conversation_id.is_none());

    assert!(matches!(
        rx.try_recv(),
        Ok(EngineControl::UpdateDispatchMode(
            SameRoleDispatchMode::StrictPrimaryFailover
        ))
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(EngineControl::UpdateSlots(slots)) if slots.len() == 3
    ));

    assert!(env.state.is_room_currently_running("room-live-1"));
    assert!(!env.state.is_room_currently_running("room-other"));
    env.state.phase = RunPhase::Completed;
    assert!(!env.state.is_room_currently_running("room-live-1"));
}
