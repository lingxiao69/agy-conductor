use crate::agy::gateway_wildcards::expand_gateway_wildcard_models;
use crate::agy::resolver::ResolvedAgyRuntime;
use crate::billing::SlotUsageStats;
use crate::orchestrator::types::{AgentSlot, RoleKind, SlotRunState};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tokio::time::{timeout, Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ModelFamily {
    Carbon,
    Neon,
    Hydrogen,
    Argon,
    Pro,
    Flash,
    Other,
}

impl ModelFamily {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Carbon => "Carbon",
            Self::Neon => "Neon",
            Self::Hydrogen => "Hydrogen",
            Self::Argon => "Argon",
            Self::Pro => "Pro",
            Self::Flash => "Flash",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningTier {
    XHigh,
    High,
    Medium,
    Low,
    Standard,
}

impl ReasoningTier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::XHigh => "XHigh",
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
            Self::Standard => "Standard",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub is_gateway: bool,
    pub clean_slug: String,
    pub family: ModelFamily,
    pub tier: ReasoningTier,
}

pub fn classify_family(clean_slug: &str) -> ModelFamily {
    let lower = clean_slug.to_ascii_lowercase();
    if lower.contains("carbon") {
        ModelFamily::Carbon
    } else if lower.contains("neon") {
        ModelFamily::Neon
    } else if lower.contains("hydrogen") {
        ModelFamily::Hydrogen
    } else if lower.contains("argon") {
        ModelFamily::Argon
    } else if lower.contains("flash") {
        ModelFamily::Flash
    } else if lower.contains("pro") {
        ModelFamily::Pro
    } else {
        ModelFamily::Other
    }
}

pub fn classify_tier(clean_slug: &str) -> ReasoningTier {
    let lower = clean_slug.to_ascii_lowercase();
    if lower.ends_with("-xhigh") || lower.contains("xhigh") {
        ReasoningTier::XHigh
    } else if lower.ends_with("-high") || lower.contains("-high-") {
        ReasoningTier::High
    } else if lower.ends_with("-medium") || lower.contains("-medium-") {
        ReasoningTier::Medium
    } else if lower.ends_with("-low") || lower.contains("-low-") {
        ReasoningTier::Low
    } else {
        ReasoningTier::Standard
    }
}

pub fn parse_models_output(stdout: &str) -> Vec<ModelInfo> {
    let mut seen = HashSet::new();
    let mut models = Vec::new();

    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((left, right)) = line.split_once('\t') else {
            continue;
        };
        let id = left.trim();
        let display_name = right.trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let is_gateway = id.starts_with("[Gateway]");
        let clean_slug = id
            .strip_prefix("[Gateway]")
            .unwrap_or(id)
            .trim()
            .to_ascii_lowercase();
        let family = classify_family(&clean_slug);
        let tier = classify_tier(&clean_slug);
        let friendly_display = if display_name.is_empty() {
            id.to_string()
        } else {
            display_name.to_string()
        };

        models.push(ModelInfo {
            id: id.to_string(),
            display_name: friendly_display,
            is_gateway,
            clean_slug,
            family,
            tier,
        });
    }

    let mut models = expand_gateway_wildcard_models(models);
    models.sort_by(|a, b| {
        a.family
            .cmp(&b.family)
            .then_with(|| a.tier.cmp(&b.tier))
            .then_with(|| b.is_gateway.cmp(&a.is_gateway))
            .then_with(|| a.clean_slug.cmp(&b.clean_slug))
    });
    models
}

pub async fn fetch_available_models(runtime: &ResolvedAgyRuntime) -> Result<Vec<ModelInfo>, String> {
    let mut cmd = runtime.build_headless_command(&["models".to_string()]);
    let output = timeout(Duration::from_secs(20), cmd.output())
        .await
        .map_err(|_| "探测本地模型列表超时 (20s)".to_string())?
        .map_err(|e| format!("执行 agy models 失败: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "agy models 执行失败 ({}): {}",
            output.status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_models_output(&stdout))
}

/// Picks the best matching model for a target family and ordered tier preferences,
/// honoring the user's Gateway vs Official source preference.
pub fn pick_best_model(
    models: &[ModelInfo],
    family: ModelFamily,
    preferred_tiers: &[ReasoningTier],
    prefer_gateway: bool,
) -> Option<String> {
    let source_order = if prefer_gateway {
        [true, false]
    } else {
        [false, true]
    };

    for &want_gateway in &source_order {
        for &tier in preferred_tiers {
            if let Some(m) = models.iter().find(|m| {
                m.family == family
                    && m.tier == tier
                    && m.is_gateway == want_gateway
                    && !m.clean_slug.contains("tiered")
                    && !m.clean_slug.contains("sonar")
            }) {
                return Some(m.id.clone());
            }
        }
        if let Some(m) = models
            .iter()
            .find(|m| m.family == family && m.is_gateway == want_gateway)
        {
            return Some(m.id.clone());
        }
    }

    models
        .iter()
        .find(|m| m.is_gateway == prefer_gateway)
        .or_else(|| models.first())
        .map(|m| m.id.clone())
}

/// Builds the default 4-slot team recommended by the specification:
/// Architect (Carbon High, P1), Coder P1 (Neon XHigh/High), Coder P2 (Hydrogen XHigh/High),
/// Reviewer (Carbon High, P1).
pub fn build_default_roster(models: &[ModelInfo], prefer_gateway: bool) -> Vec<AgentSlot> {
    let carbon = pick_best_model(
        models,
        ModelFamily::Carbon,
        &[ReasoningTier::High, ReasoningTier::Medium],
        prefer_gateway,
    )
    .unwrap_or_else(|| {
        if prefer_gateway {
            "[Gateway] gemini-4-carbon-high".to_string()
        } else {
            "gemini-4-carbon-high".to_string()
        }
    });

    let neon = pick_best_model(
        models,
        ModelFamily::Neon,
        &[ReasoningTier::XHigh, ReasoningTier::High],
        prefer_gateway,
    )
    .unwrap_or_else(|| {
        if prefer_gateway {
            "[Gateway] gemini-4-neon-xhigh".to_string()
        } else {
            "gemini-4-neon-xhigh".to_string()
        }
    });

    let hydrogen = pick_best_model(
        models,
        ModelFamily::Hydrogen,
        &[ReasoningTier::XHigh, ReasoningTier::High],
        prefer_gateway,
    )
    .unwrap_or_else(|| {
        if prefer_gateway {
            "[Gateway] gemini-4-hydrogen-xhigh".to_string()
        } else {
            "gemini-4-hydrogen-xhigh".to_string()
        }
    });

    vec![
        AgentSlot {
            slot_id: "arch-carbon".to_string(),
            alias: "carbon-arch".to_string(),
            display_name: "总架构师 · Carbon".to_string(),
            model_id: carbon.clone(),
            role: RoleKind::Architect,
            priority: 1,
            enabled: true,
            state: SlotRunState::Idle,
            conversation_id: None,
            session_reused: false,
            current_activity: None,
            consecutive_failures: 0,
            usage: SlotUsageStats::for_model(&carbon),
        },
        AgentSlot {
            slot_id: "coder-neon".to_string(),
            alias: "neon".to_string(),
            display_name: "编码程序员 · Neon".to_string(),
            model_id: neon.clone(),
            role: RoleKind::Coder,
            priority: 1,
            enabled: true,
            state: SlotRunState::Idle,
            conversation_id: None,
            session_reused: false,
            current_activity: None,
            consecutive_failures: 0,
            usage: SlotUsageStats::for_model(&neon),
        },
        AgentSlot {
            slot_id: "coder-hydrogen".to_string(),
            alias: "hydrogen".to_string(),
            display_name: "编码程序员 · Hydrogen".to_string(),
            model_id: hydrogen.clone(),
            role: RoleKind::Coder,
            priority: 2,
            enabled: true,
            state: SlotRunState::Idle,
            conversation_id: None,
            session_reused: false,
            current_activity: None,
            consecutive_failures: 0,
            usage: SlotUsageStats::for_model(&hydrogen),
        },
        AgentSlot {
            slot_id: "rev-carbon".to_string(),
            alias: "carbon-rev".to_string(),
            display_name: "代码审查员 · Carbon".to_string(),
            model_id: carbon.clone(),
            role: RoleKind::Reviewer,
            priority: 1,
            enabled: true,
            state: SlotRunState::Idle,
            conversation_id: None,
            session_reused: false,
            current_activity: None,
            consecutive_failures: 0,
            usage: SlotUsageStats::for_model(&carbon),
        },
    ]
}
