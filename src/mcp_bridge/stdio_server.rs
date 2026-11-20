use crate::orchestrator::types::{GroupChatMessage, MessageBadge, RoleKind};
use crate::storage::store::AppStorage;
use chrono::Local;
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const MAX_MCP_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct McpBridgeConfig {
    pub room_dir: PathBuf,
    pub default_alias: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramingMode {
    LineDelimited,
    ContentLength,
}

/// Runs the synchronous MCP JSON-RPC 2.0 stdio bridge so external interactive `agy` CLI
/// sessions can read, post, and wait on messages in the shared AgyConductor `board.jsonl`.
pub fn run_mcp_stdio_server(config: McpBridgeConfig) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    while let Some((raw, framing)) = read_next_rpc_message(&mut reader)? {
        let Ok(req) = serde_json::from_str::<Value>(&raw) else {
            let err = json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": "Parse error" }
            });
            write_rpc_message(&mut writer, &err, framing)?;
            continue;
        };

        if let Some(resp) = handle_rpc_request(&config, &req) {
            write_rpc_message(&mut writer, &resp, framing)?;
        }
    }

    Ok(())
}

pub fn read_next_rpc_message<R: BufRead>(
    reader: &mut R,
) -> Result<Option<(String, FramingMode)>, String> {
    let mut first_line = String::new();
    loop {
        first_line.clear();
        let n = reader
            .read_line(&mut first_line)
            .map_err(|e| format!("读取 MCP stdin 失败: {e}"))?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = first_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('{') {
            if trimmed.len() > MAX_MCP_FRAME_BYTES {
                return Err(format!(
                    "MCP 单行消息大小 {} 字节超过上限 ({} 字节)",
                    trimmed.len(),
                    MAX_MCP_FRAME_BYTES
                ));
            }
            return Ok(Some((trimmed.to_string(), FramingMode::LineDelimited)));
        }

        // Handle LSP / MCP Content-Length headers.
        let mut content_length: Option<usize> = parse_content_length(trimmed);
        let mut header_line = String::new();
        loop {
            header_line.clear();
            let hn = reader
                .read_line(&mut header_line)
                .map_err(|e| format!("读取 MCP Header 失败: {e}"))?;
            if hn == 0 || header_line.trim().is_empty() {
                break;
            }
            if content_length.is_none() {
                content_length = parse_content_length(header_line.trim());
            }
        }
        let Some(len) = content_length else {
            continue;
        };
        if len > MAX_MCP_FRAME_BYTES {
            return Err(format!(
                "MCP Content-Length ({len} 字节) 超过安全上限 ({MAX_MCP_FRAME_BYTES} 字节)"
            ));
        }
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("读取 MCP Body 失败: {e}"))?;
        let text = String::from_utf8(buf)
            .map_err(|e| format!("MCP Body 非有效 UTF-8: {e}"))?;
        return Ok(Some((text, FramingMode::ContentLength)));
    }
}

fn parse_content_length(header: &str) -> Option<usize> {
    let (name, val) = header.split_once(':')?;
    if name.trim().eq_ignore_ascii_case("content-length") {
        val.trim().parse::<usize>().ok()
    } else {
        None
    }
}

fn write_rpc_message<W: Write>(
    writer: &mut W,
    payload: &Value,
    framing: FramingMode,
) -> Result<(), String> {
    let body = serde_json::to_string(payload)
        .map_err(|e| format!("序列化 MCP 响应失败: {e}"))?;
    match framing {
        FramingMode::LineDelimited => {
            writeln!(writer, "{body}").map_err(|e| format!("写入 MCP stdout 失败: {e}"))?;
        }
        FramingMode::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)
                .map_err(|e| format!("写入 MCP Framed stdout 失败: {e}"))?;
        }
    }
    writer
        .flush()
        .map_err(|e| format!("刷新 MCP stdout 失败: {e}"))
}

pub fn handle_rpc_request(config: &McpBridgeConfig, req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(|v| v.as_str())?;
    let id = req.get("id").cloned();

    // JSON-RPC notifications omit `id` and expect no response.
    if id.is_none() {
        return None;
    }
    let id = id.unwrap_or(Value::Null);

    match method {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "agy-conductor-hub",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        })),
        "ping" => Some(json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": tool_definitions() }
        })),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let tool_result = dispatch_tool_call(config, name, &args);
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": tool_result
            }))
        }
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        })),
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "hub_post_message",
            "description": "向 AgyConductor 群组消息板发送一条消息（支持 @其他角色）",
            "inputSchema": {
                "type": "object",
                "required": ["content"],
                "properties": {
                    "sender_alias": { "type": "string", "description": "发送者别名，缺省使用当前 CLI 槽位别名" },
                    "content": { "type": "string", "description": "群消息内容" },
                    "task_id": { "type": "string", "description": "关联的子任务 ID（可选）" }
                }
            }
        },
        {
            "name": "hub_read_messages",
            "description": "读取 AgyConductor 群组消息板上的最新消息列表",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "after_index": { "type": "integer", "description": "仅返回该序号之后的新消息（从 0 开始）" },
                    "limit": { "type": "integer", "description": "最多返回条数，默认 30" }
                }
            }
        },
        {
            "name": "hub_wait_message",
            "description": "阻塞等待群组消息板出现新消息（可选按 @mention 过滤）",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "after_index": { "type": "integer", "description": "已知最后消息总数，等待更大索引" },
                    "mention": { "type": "string", "description": "仅唤醒包含 @mention 的消息（可选）" },
                    "timeout_seconds": { "type": "integer", "description": "最长等待秒数（默认 30，上限 120）" }
                }
            }
        },
        {
            "name": "hub_get_status",
            "description": "读取当前群组项目的目标、阶段与全部子任务进度快照",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

fn dispatch_tool_call(config: &McpBridgeConfig, name: &str, args: &Value) -> Value {
    match name {
        "hub_post_message" => tool_post_message(config, args),
        "hub_read_messages" => tool_read_messages(&config.room_dir, args),
        "hub_wait_message" => tool_wait_message(&config.room_dir, args),
        "hub_get_status" => tool_get_status(&config.room_dir),
        _ => json!({
            "isError": true,
            "content": [{ "type": "text", "text": format!("未知工具: {name}") }]
        }),
    }
}

fn tool_post_message(config: &McpBridgeConfig, args: &Value) -> Value {
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if content.is_empty() {
        return json!({
            "isError": true,
            "content": [{ "type": "text", "text": "content 不能为空" }]
        });
    }
    let alias = args
        .get("sender_alias")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&config.default_alias)
        .to_string();
    let task_id = args
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);

    let msg = GroupChatMessage {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Local::now().format("%H:%M:%S").to_string(),
        sender_slot_id: Some(format!("cli-{alias}")),
        sender_alias: alias.clone(),
        sender_display: format!("外部 CLI · {alias}"),
        sender_role: Some(RoleKind::Coder),
        priority: None,
        is_user: false,
        is_system: false,
        content: content.to_string(),
        badge: MessageBadge::Info,
        task_id,
        modified_files: Vec::new(),
        duration_seconds: None,
        tool_steps: None,
        issues: Vec::new(),
    };

    match AppStorage::append_board_message(&config.room_dir, &msg) {
        Ok(()) => json!({
            "content": [{ "type": "text", "text": format!("已发送到群组消息板 (id={})", msg.id) }]
        }),
        Err(e) => json!({
            "isError": true,
            "content": [{ "type": "text", "text": format!("发送失败: {e}") }]
        }),
    }
}

fn tool_read_messages(room_dir: &Path, args: &Value) -> Value {
    let after = args
        .get("after_index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 200) as usize;

    match AppStorage::read_board_messages(room_dir) {
        Ok(all) => {
            let total = all.len();
            let slice: Vec<&GroupChatMessage> = all.iter().skip(after).take(limit).collect();
            let text = serde_json::to_string_pretty(&json!({
                "total": total,
                "messages": slice
            }))
            .unwrap_or_else(|_| "{}".to_string());
            json!({ "content": [{ "type": "text", "text": text }] })
        }
        Err(e) => json!({
            "isError": true,
            "content": [{ "type": "text", "text": e }]
        }),
    }
}

fn tool_wait_message(room_dir: &Path, args: &Value) -> Value {
    let after = args
        .get("after_index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let mention = args
        .get("mention")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().trim_start_matches('@').to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let wait_secs = args
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 120);

    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        if let Ok(all) = AppStorage::read_board_messages(room_dir) {
            let matched: Vec<&GroupChatMessage> = all
                .iter()
                .skip(after)
                .filter(|m| match &mention {
                    Some(target) => m.content.to_ascii_lowercase().contains(&format!("@{target}")),
                    None => true,
                })
                .collect();
            if !matched.is_empty() {
                let text = serde_json::to_string_pretty(&json!({
                    "total": all.len(),
                    "matched": matched
                }))
                .unwrap_or_else(|_| "{}".to_string());
                return json!({ "content": [{ "type": "text", "text": text }] });
            }
        }
        if Instant::now() >= deadline {
            return json!({
                "content": [{ "type": "text", "text": "{\"timed_out\":true,\"matched\":[]}" }]
            });
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn tool_get_status(room_dir: &Path) -> Value {
    match AppStorage::read_room_snapshot(room_dir) {
        Some(snap) => {
            let text = serde_json::to_string_pretty(&snap).unwrap_or_else(|_| "{}".to_string());
            json!({ "content": [{ "type": "text", "text": text }] })
        }
        None => json!({
            "content": [{ "type": "text", "text": "{\"status\":\"no_active_snapshot\"}" }]
        }),
    }
}
