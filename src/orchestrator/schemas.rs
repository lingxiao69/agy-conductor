use crate::orchestrator::types::ReviewIssue;
use crate::storage::store::atomic_write_text;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedTaskSpec {
    pub task_id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub owned_paths: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub preferred_coder_alias: Option<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchitectOutput {
    pub action: String,
    pub chat_message: String,
    #[serde(default)]
    pub user_plain_explanation: Option<String>,
    #[serde(default)]
    pub architecture_notes: String,
    #[serde(default)]
    pub verification_commands: Vec<String>,
    #[serde(default)]
    pub tasks: Vec<PlannedTaskSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoderOutput {
    pub status: String,
    pub chat_message: String,
    #[serde(default)]
    pub modified_files: Vec<String>,
    #[serde(default)]
    pub self_check_result: String,
    #[serde(default)]
    pub blocker_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewerOutput {
    pub verdict: String,
    pub chat_message: String,
    #[serde(default)]
    pub commands_verified: Vec<String>,
    #[serde(default)]
    pub issues: Vec<ReviewIssue>,
}

#[derive(Debug, Clone)]
pub struct RoleSchemaFiles {
    pub architect: PathBuf,
    pub coder: PathBuf,
    pub reviewer: PathBuf,
}

pub fn architect_schema_value() -> Value {
    json!({
        "type": "object",
        "required": ["action", "chat_message", "architecture_notes", "tasks"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["DISPATCH_PLAN", "FINAL_ACCEPT", "NEED_USER_CLARIFICATION"]
            },
            "chat_message": {
                "type": "string",
                "description": "发往项目群组的自然语言规划说明，包含 @程序员 派单指令；向用户提问时必须使用通俗大白话"
            },
            "user_plain_explanation": {
                "type": ["string", "null"],
                "description": "当 action 为 NEED_USER_CLARIFICATION 时必填：面向不懂代码细节的项目负责人的通俗大白话说明（讲清卡住的功能、大白话原因及带编号的回复选项）"
            },
            "architecture_notes": {
                "type": "string",
                "description": "全局架构约束、模块接口契约与工程红线提示"
            },
            "verification_commands": {
                "type": "array",
                "items": { "type": "string" }
            },
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": [
                        "task_id",
                        "title",
                        "description",
                        "owned_paths",
                        "depends_on",
                        "acceptance_criteria"
                    ],
                    "properties": {
                        "task_id": { "type": "string" },
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "owned_paths": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "depends_on": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "preferred_coder_alias": {
                            "type": ["string", "null"]
                        },
                        "acceptance_criteria": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                }
            }
        }
    })
}

pub fn coder_schema_value() -> Value {
    json!({
        "type": "object",
        "required": ["status", "chat_message", "modified_files", "self_check_result"],
        "properties": {
            "status": {
                "type": "string",
                "enum": ["SUBMIT_FOR_REVIEW", "BLOCKED_BY_ARCH"]
            },
            "chat_message": {
                "type": "string",
                "description": "发往群组的完成汇报，需 @审查员 提请代码审查"
            },
            "modified_files": {
                "type": "array",
                "items": { "type": "string" }
            },
            "self_check_result": {
                "type": "string",
                "description": "自测命令与运行结果摘要"
            },
            "blocker_reason": {
                "type": ["string", "null"]
            }
        }
    })
}

pub fn reviewer_schema_value() -> Value {
    json!({
        "type": "object",
        "required": ["verdict", "chat_message", "issues", "commands_verified"],
        "properties": {
            "verdict": {
                "type": "string",
                "enum": ["APPROVED", "CHANGES_REQUESTED"]
            },
            "chat_message": {
                "type": "string",
                "description": "发往群组的审查结论：不通过时 @对应程序员 列出具体问题；通过时告知群组验收合格"
            },
            "commands_verified": {
                "type": "array",
                "items": { "type": "string" }
            },
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["severity", "file_path", "detail"],
                    "properties": {
                        "severity": {
                            "type": "string",
                            "enum": ["CRITICAL", "MAJOR", "MINOR"]
                        },
                        "file_path": { "type": "string" },
                        "line_hint": { "type": ["string", "null"] },
                        "detail": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// Writes the three JSON Schema files to disk so `--json-schema <absolute_path>`
/// can be passed safely without hitting Windows command-line length or quoting limits.
pub fn write_role_schemas(dir: &Path) -> Result<RoleSchemaFiles, String> {
    let architect = dir.join("architect_schema.json");
    let coder = dir.join("coder_schema.json");
    let reviewer = dir.join("reviewer_schema.json");

    atomic_write_text(
        &architect,
        &serde_json::to_string_pretty(&architect_schema_value())
            .map_err(|e| format!("序列化 Architect Schema 失败: {e}"))?,
    )?;
    atomic_write_text(
        &coder,
        &serde_json::to_string_pretty(&coder_schema_value())
            .map_err(|e| format!("序列化 Coder Schema 失败: {e}"))?,
    )?;
    atomic_write_text(
        &reviewer,
        &serde_json::to_string_pretty(&reviewer_schema_value())
            .map_err(|e| format!("序列化 Reviewer Schema 失败: {e}"))?,
    )?;

    Ok(RoleSchemaFiles {
        architect,
        coder,
        reviewer,
    })
}
