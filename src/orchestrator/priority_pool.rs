use crate::orchestrator::types::{AgentSlot, RoleKind, SameRoleDispatchMode, SlotRunState};

pub const MAX_CODER_STREAK_BEFORE_HANDOFF: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotSelection {
    /// Immediately dispatchable slot.
    Selected { slot_id: String, is_handoff: bool },
    /// In `StrictPrimaryFailover`, the highest-priority healthy slot is currently `Working`;
    /// queue the task rather than waking lower-priority backup slots.
    WaitForBusyPrimary { primary_slot_id: String },
    /// For sticky rework, the original coder is busy on another task; keep waiting for it.
    WaitForOriginalCoder { original_slot_id: String },
    /// All enabled slots for this role are currently busy (`Working`).
    AllBusy,
    /// No enabled healthy slot is available (all disabled, quota cooling, paused, or errored).
    NoneAvailable,
}

/// Selects the best available slot for a new task under the configured same-role strategy.
pub fn select_slot_for_new_task(
    role: RoleKind,
    explicit_mention: Option<&str>,
    slots: &[AgentSlot],
    mode: SameRoleDispatchMode,
) -> SlotSelection {
    let candidates: Vec<&AgentSlot> = slots
        .iter()
        .filter(|s| s.enabled && s.role == role)
        .collect();

    if candidates.is_empty() {
        return SlotSelection::NoneAvailable;
    }

    // 1. Explicit @alias mention takes precedence if belonging to the target role and Idle.
    if let Some(mention) = explicit_mention
        .map(|m| m.trim().trim_start_matches('@'))
        .filter(|m| !m.is_empty())
    {
        if let Some(slot) = candidates.iter().find(|s| {
            (s.alias.eq_ignore_ascii_case(mention) || s.slot_id.eq_ignore_ascii_case(mention))
                && s.state == SlotRunState::Idle
        }) {
            return SlotSelection::Selected {
                slot_id: slot.slot_id.clone(),
                is_handoff: false,
            };
        }
    }

    // Healthy pool: exclude QuotaCooling / Paused / ExternalCliAttached / Error.
    let healthy: Vec<&AgentSlot> = candidates
        .iter()
        .copied()
        .filter(|s| !s.state.is_unavailable())
        .collect();

    if healthy.is_empty() {
        return SlotSelection::NoneAvailable;
    }

    match mode {
        SameRoleDispatchMode::StrictPrimaryFailover => {
            let best_priority = healthy
                .iter()
                .map(|s| s.normalized_priority())
                .min()
                .unwrap_or(99);
            let mut primary_group: Vec<&AgentSlot> = healthy
                .iter()
                .copied()
                .filter(|s| s.normalized_priority() == best_priority)
                .collect();
            primary_group.sort_by(|a, b| {
                a.consecutive_failures
                    .cmp(&b.consecutive_failures)
                    .then_with(|| a.slot_id.cmp(&b.slot_id))
            });

            if let Some(idle_primary) = primary_group
                .iter()
                .find(|s| s.state == SlotRunState::Idle)
            {
                return SlotSelection::Selected {
                    slot_id: idle_primary.slot_id.clone(),
                    is_handoff: false,
                };
            }

            if let Some(busy_primary) = primary_group
                .iter()
                .find(|s| s.state == SlotRunState::Working)
            {
                return SlotSelection::WaitForBusyPrimary {
                    primary_slot_id: busy_primary.slot_id.clone(),
                };
            }

            SlotSelection::NoneAvailable
        }
        SameRoleDispatchMode::PriorityWithOverflow => {
            let mut idle: Vec<&AgentSlot> = healthy
                .iter()
                .copied()
                .filter(|s| s.state == SlotRunState::Idle)
                .collect();

            if idle.is_empty() {
                return if healthy.iter().any(|s| s.state == SlotRunState::Working) {
                    SlotSelection::AllBusy
                } else {
                    SlotSelection::NoneAvailable
                };
            }

            idle.sort_by(|a, b| {
                a.normalized_priority()
                    .cmp(&b.normalized_priority())
                    .then_with(|| a.consecutive_failures.cmp(&b.consecutive_failures))
                    .then_with(|| a.slot_id.cmp(&b.slot_id))
            });

            SlotSelection::Selected {
                slot_id: idle[0].slot_id.clone(),
                is_handoff: false,
            }
        }
    }
}

/// Selects the coder slot when a reviewer returns `CHANGES_REQUESTED`.
/// Preserves conversation context stickiness with the original author while seamlessly
/// failing over to the next highest-priority healthy coder when the original author is
/// quota-exhausted, paused, attached to external CLI, or hits repeated failures.
pub fn select_slot_for_rework(
    last_coder_slot_id: Option<&str>,
    coder_failure_streak: u8,
    slots: &[AgentSlot],
    mode: SameRoleDispatchMode,
) -> SlotSelection {
    let should_force_handoff = coder_failure_streak >= MAX_CODER_STREAK_BEFORE_HANDOFF;

    if !should_force_handoff {
        if let Some(orig_id) = last_coder_slot_id {
            if let Some(orig) = slots
                .iter()
                .find(|s| s.slot_id == orig_id && s.enabled && s.role == RoleKind::Coder)
            {
                match orig.state {
                    SlotRunState::Idle => {
                        return SlotSelection::Selected {
                            slot_id: orig.slot_id.clone(),
                            is_handoff: false,
                        };
                    }
                    SlotRunState::Working => {
                        return SlotSelection::WaitForOriginalCoder {
                            original_slot_id: orig.slot_id.clone(),
                        };
                    }
                    SlotRunState::QuotaCooling
                    | SlotRunState::Paused
                    | SlotRunState::ExternalCliAttached
                    | SlotRunState::Error => {
                        // Fall through to cross-model handoff below.
                    }
                }
            }
        }
    }

    // Cross-model handoff: exclude the failing original slot when other coders exist.
    let filtered_slots: Vec<AgentSlot> = slots
        .iter()
        .filter(|s| {
            if should_force_handoff {
                Some(s.slot_id.as_str()) != last_coder_slot_id
            } else {
                true
            }
        })
        .cloned()
        .collect();

    let fallback = select_slot_for_new_task(RoleKind::Coder, None, &filtered_slots, mode);
    match fallback {
        SlotSelection::Selected { slot_id, .. } => SlotSelection::Selected {
            is_handoff: Some(slot_id.as_str()) != last_coder_slot_id,
            slot_id,
        },
        other => {
            // If excluding the original slot left no alternative at all, retry full pool.
            if should_force_handoff {
                match select_slot_for_new_task(RoleKind::Coder, None, slots, mode) {
                    SlotSelection::Selected { slot_id, .. } => SlotSelection::Selected {
                        is_handoff: Some(slot_id.as_str()) != last_coder_slot_id,
                        slot_id,
                    },
                    res => res,
                }
            } else {
                other
            }
        }
    }
}
