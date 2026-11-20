use crate::agy::models::{classify_family, classify_tier, ModelFamily, ModelInfo, ReasoningTier};
use crate::billing::strip_model_prefix;
use crate::storage::store::atomic_write_text;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// 默认只读本机 `agy` 用户配置文件路径（严禁修改原文件）。
pub fn default_agy_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".gemini").join("antigravity-cli").join("settings.json"))
}

/// 在 `parse_models_output` 排序前拦截并展开 `[Gateway] <prefix>*` 通配符型号，
/// 同时彻底剔除任何包含 `*` 的 `ModelInfo`，防止脏型号进入下拉列表或默认编队。
pub fn expand_gateway_wildcard_models(raw_models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    let mut wildcard_prefixes: Vec<String> = Vec::new();
    let mut seen_prefixes: HashSet<String> = HashSet::new();

    for m in &raw_models {
        if m.is_gateway && m.clean_slug.contains('*') {
            let prefix = normalize_wildcard_prefix(&m.clean_slug);
            if !prefix.is_empty() && seen_prefixes.insert(prefix.clone()) {
                wildcard_prefixes.push(prefix);
            }
        }
    }

    let official_concrete: Vec<ModelInfo> = raw_models
        .iter()
        .filter(|m| !m.is_gateway && !m.id.contains('*') && !m.clean_slug.contains('*'))
        .cloned()
        .collect();

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut out: Vec<ModelInfo> = Vec::new();

    for m in raw_models {
        if m.id.contains('*') || m.clean_slug.contains('*') {
            continue;
        }
        if seen_ids.insert(m.id.clone()) {
            out.push(m);
        }
    }

    for prefix in wildcard_prefixes {
        let dot_prefix = format!("{prefix}.");
        let dash_prefix = format!("{prefix}-");
        let matched_slugs: Vec<String> = official_concrete
            .iter()
            .filter(|m| {
                m.clean_slug == prefix
                    || m.clean_slug.starts_with(&dash_prefix)
                    || m.clean_slug.starts_with(&dot_prefix)
            })
            .map(|m| m.clean_slug.clone())
            .collect();

        let expanded_slugs = if !matched_slugs.is_empty() {
            matched_slugs
        } else {
            fallback_expanded_slugs(&prefix)
        };

        for slug in expanded_slugs {
            let id = format!("[Gateway] {slug}");
            if !seen_ids.insert(id.clone()) {
                continue;
            }
            let family = classify_family(&slug);
            let tier = classify_tier(&slug);
            out.push(ModelInfo {
                display_name: id.clone(),
                id,
                is_gateway: true,
                clean_slug: slug,
                family,
                tier,
            });
        }
    }

    out.retain(|m| !m.id.contains('*') && !m.clean_slug.contains('*'));
    out
}

/// 将已污染的通配符型号（如 `"[Gateway] gemini-4-carbon*"`）自动自愈为安全具体推荐型号。
pub fn sanitize_wildcard_model_id(raw: &str) -> String {
    if !raw.contains('*') {
        return raw.to_string();
    }
    let trimmed = raw.trim();
    let is_gateway = trimmed.starts_with("[Gateway]");
    let base = normalize_wildcard_prefix(&strip_model_prefix(trimmed).to_ascii_lowercase());

    let concrete_slug = if base.is_empty() {
        "gemini-4-carbon-high".to_string()
    } else if classify_tier(&base) != ReasoningTier::Standard {
        base
    } else {
        match classify_family(&base) {
            ModelFamily::Neon | ModelFamily::Hydrogen => format!("{base}-xhigh"),
            ModelFamily::Carbon | ModelFamily::Argon | ModelFamily::Pro | ModelFamily::Flash => {
                format!("{base}-high")
            }
            ModelFamily::Other => base,
        }
    };

    if is_gateway {
        format!("[Gateway] {concrete_slug}")
    } else {
        concrete_slug
    }
}

/// 为无头执行与外部交互终端动态生成会话级 `--settings <file.json>`：
/// - 只读读取本机 `~/.gemini/antigravity-cli/settings.json`，严禁修改原文件；
/// - 当所选 `[Gateway] <slug>` 在原 `customModels` 中缺失或仅存在 `*` 通配符时，
///   在 `output_dir` 原子生成包含具体 `models/<slug>` 映射的会话级配置文件；
/// - 仅对 `-low` / `-medium` / `-high` 写入 `thinkingLevel`，`-xhigh` 等严禁写该字段
///   以兼容 Go 枚举解析并交由本机 `thinking-proxy.mjs` 自动注入；
/// - 可同时合并 `extra_mcp_servers` 写入顶层 `mcpServers`，避免多 `--settings` 参数冲突。
pub fn prepare_gateway_session_settings(
    model_id: &str,
    output_dir: &Path,
    file_stem: &str,
    extra_mcp_servers: Option<Value>,
    agy_settings_path_override: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let sanitized = sanitize_wildcard_model_id(model_id);
    let trimmed = sanitized.trim();
    let is_gateway = trimmed.starts_with("[Gateway]");

    if !is_gateway && extra_mcp_servers.is_none() {
        return Ok(None);
    }

    let settings_path = agy_settings_path_override
        .map(PathBuf::from)
        .or_else(default_agy_settings_path);
    let orig_root: Map<String, Value> = settings_path
        .as_deref()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str::<Value>(raw.trim()).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let orig_custom_models: Map<String, Value> = orig_root
        .get("customModelsConfig")
        .and_then(|c| c.get("customModels"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let clean_slug = if is_gateway {
        strip_model_prefix(trimmed).trim().to_ascii_lowercase()
    } else {
        String::new()
    };

    let has_concrete_entry = is_gateway
        && !clean_slug.is_empty()
        && orig_custom_models.iter().any(|(key, val)| {
            let matches_target = key.eq_ignore_ascii_case(&clean_slug)
                || val
                    .get("label")
                    .and_then(Value::as_str)
                    .is_some_and(|lbl| lbl.eq_ignore_ascii_case(trimmed));
            matches_target
                && val
                    .get("modelName")
                    .and_then(Value::as_str)
                    .is_some_and(|mn| !mn.trim().is_empty() && !mn.contains('*'))
        });

    if (!is_gateway || has_concrete_entry) && extra_mcp_servers.is_none() {
        return Ok(None);
    }

    let mut session_root = Map::new();

    if is_gateway && !clean_slug.is_empty() {
        let mut filtered_models = Map::new();
        for (k, v) in orig_custom_models {
            let model_name_bad = v
                .get("modelName")
                .and_then(Value::as_str)
                .is_some_and(|mn| mn.contains('*'));
            let label_bad = v
                .get("label")
                .and_then(Value::as_str)
                .is_some_and(|lbl| lbl.contains('*'));
            if k.contains('*') || model_name_bad || label_bad {
                continue;
            }
            filtered_models.insert(k, v);
        }

        if !has_concrete_entry {
            filtered_models.insert(clean_slug.clone(), build_gateway_model_entry(&clean_slug));
        }

        session_root.insert(
            "customModelsConfig".to_string(),
            json!({ "customModels": Value::Object(filtered_models) }),
        );
        if let Some(provider) = orig_root.get("modelProvider").cloned() {
            session_root.insert("modelProvider".to_string(), provider);
        }
    }

    if let Some(extra_mcp) = extra_mcp_servers {
        let merged_mcp = merge_mcp_servers(orig_root.get("mcpServers"), extra_mcp);
        session_root.insert("mcpServers".to_string(), merged_mcp);
    }

    fs::create_dir_all(output_dir)
        .map_err(|e| format!("创建会话配置目录失败 {}: {e}", output_dir.display()))?;

    let safe_stem = sanitize_stem(file_stem);
    let target_path = output_dir.join(format!("session-settings-{safe_stem}.json"));
    let text = serde_json::to_string_pretty(&Value::Object(session_root))
        .map_err(|e| format!("序列化会话级 settings.json 失败: {e}"))?;
    atomic_write_text(&target_path, &text)?;
    Ok(Some(target_path))
}

fn build_gateway_model_entry(clean_slug: &str) -> Value {
    let mut entry = Map::new();
    entry.insert(
        "label".to_string(),
        Value::String(format!("[Gateway] {clean_slug}")),
    );
    entry.insert(
        "modelName".to_string(),
        Value::String(format!("models/{clean_slug}")),
    );
    entry.insert(
        "apiProvider".to_string(),
        Value::String("API_PROVIDER_GOOGLE_GEMINI".to_string()),
    );
    entry.insert("maxTokens".to_string(), json!(1_048_576));
    entry.insert("maxOutputTokens".to_string(), json!(65_536));
    if let Some(level) = supported_go_thinking_level(clean_slug) {
        entry.insert("thinkingLevel".to_string(), Value::String(level.to_string()));
    }
    Value::Object(entry)
}

fn supported_go_thinking_level(clean_slug: &str) -> Option<&'static str> {
    let lower = clean_slug.trim().to_ascii_lowercase();
    if lower.ends_with("-low") {
        Some("LOW")
    } else if lower.ends_with("-medium") {
        Some("MEDIUM")
    } else if lower.ends_with("-high") {
        Some("HIGH")
    } else {
        None
    }
}

fn merge_mcp_servers(orig: Option<&Value>, extra: Value) -> Value {
    let Some(extra_obj) = extra.as_object() else {
        return extra;
    };
    let mut merged = orig
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (k, v) in extra_obj {
        merged.insert(k.clone(), v.clone());
    }
    Value::Object(merged)
}

fn normalize_wildcard_prefix(raw_slug: &str) -> String {
    raw_slug
        .replace('*', "")
        .trim()
        .trim_end_matches('-')
        .trim()
        .to_string()
}

fn fallback_expanded_slugs(prefix: &str) -> Vec<String> {
    let tiers: &[&str] = match classify_family(prefix) {
        ModelFamily::Neon | ModelFamily::Hydrogen => &["xhigh", "high", "medium", "low"],
        ModelFamily::Carbon | ModelFamily::Argon | ModelFamily::Flash | ModelFamily::Pro => {
            &["high", "medium", "low"]
        }
        ModelFamily::Other => &[],
    };

    if tiers.is_empty() {
        return vec![prefix.to_string()];
    }
    tiers.iter().map(|t| format!("{prefix}-{t}")).collect()
}

fn sanitize_stem(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned
    }
}
