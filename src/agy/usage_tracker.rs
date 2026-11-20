use crate::agy::stream_parser::{ParsedStreamEvent, TokenUsage};
use crate::storage::brain_health::{
    extract_step_token_quad, is_safe_conv_id, transcript_path_for_conv,
};
use crate::storage::brain_scanner::default_agy_brain_dir;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub const DEFAULT_FIRST_PROGRESS_WATCHDOG_SECS: u64 = 45;

/// 基于 `transcript.jsonl` 字节偏移的轮次内实时 Token 增量追踪器：
/// - 沿用旧会话时，以轮次启动瞬间的文件长度作为 `start_offset`，绝不重复计入旧历史步骤；
/// - 全新会话时，从偏移 `0` 开始实时读取每一步模型规划（`PLANNER_RESPONSE`）与检查点消耗；
/// - 正常收尾与 `result.usage` 按逐项最大值权威对齐，中途取消/超时可导出已发生的部分消耗。
#[derive(Debug, Clone)]
pub struct TurnTranscriptUsageTracker {
    brain_dir: Option<PathBuf>,
    conv_id: Option<String>,
    transcript_path: Option<PathBuf>,
    last_offset: u64,
    usage: TokenUsage,
    peak_context_tokens: u64,
    step_payload_usage: TokenUsage,
}

impl TurnTranscriptUsageTracker {
    pub fn new(brain_dir_override: Option<PathBuf>, initial_conv_id: Option<&str>) -> Self {
        let brain_dir = brain_dir_override.or_else(default_agy_brain_dir);
        let mut tracker = Self {
            brain_dir,
            conv_id: None,
            transcript_path: None,
            last_offset: 0,
            usage: TokenUsage::default(),
            peak_context_tokens: 0,
            step_payload_usage: TokenUsage::default(),
        };

        if let Some(cid) = initial_conv_id.map(str::trim).filter(|s| !s.is_empty()) {
            tracker.attach_initial_existing(cid);
        }
        tracker
    }

    fn attach_initial_existing(&mut self, conv_id: &str) {
        if !is_safe_conv_id(conv_id) {
            return;
        }
        self.conv_id = Some(conv_id.to_string());
        if let Some(ref bdir) = self.brain_dir {
            if let Some(path) = transcript_path_for_conv(bdir, conv_id) {
                let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                self.transcript_path = Some(path);
                self.last_offset = len;
            }
        }
    }

    /// 收到 `Init` 或 `StepUpdate` 的 `conversation_id` 时绑定追踪目标：
    /// - 若与启动时已知沿用会话一致，保留启动前记录的 `start_offset`；
    /// - 若为全新会话（或首次落盘创建），从字节偏移 `0` 开始读取本轮全部新增步骤。
    pub fn attach_conversation(&mut self, conv_id: &str) {
        let cid = conv_id.trim();
        if cid.is_empty() || !is_safe_conv_id(cid) {
            return;
        }
        let same_conv = self.conv_id.as_deref() == Some(cid);
        if !same_conv {
            self.conv_id = Some(cid.to_string());
            self.transcript_path = None;
            self.last_offset = 0;
        }
        if self.transcript_path.is_none() {
            if let Some(ref bdir) = self.brain_dir {
                if let Some(path) = transcript_path_for_conv(bdir, cid) {
                    self.transcript_path = Some(path);
                }
            }
        }
    }

    /// 兼容从 `step_update` 流载荷自带的 `usage` 字段增量合并。
    pub fn merge_step_payload_usage(&mut self, incoming: &TokenUsage) -> bool {
        let mut changed = false;
        if incoming.input_tokens > self.step_payload_usage.input_tokens {
            self.step_payload_usage.input_tokens = incoming.input_tokens;
            changed = true;
        }
        if incoming.cache_read_tokens > self.step_payload_usage.cache_read_tokens {
            self.step_payload_usage.cache_read_tokens = incoming.cache_read_tokens;
            changed = true;
        }
        if incoming.output_tokens > self.step_payload_usage.output_tokens {
            self.step_payload_usage.output_tokens = incoming.output_tokens;
            changed = true;
        }
        if incoming.thinking_tokens > self.step_payload_usage.thinking_tokens {
            self.step_payload_usage.thinking_tokens = incoming.thinking_tokens;
            changed = true;
        }
        if changed {
            let ctx = incoming
                .input_tokens
                .saturating_add(incoming.cache_read_tokens);
            self.peak_context_tokens = self.peak_context_tokens.max(ctx);
        }
        changed
    }

    /// 只读增量读取 `transcript.jsonl` 自 `last_offset` 之后的新增完整 JSON 行。
    /// 若本轮 Token 统计有实质变化，返回最新的 `(TokenUsage, peak_context_tokens)`。
    pub fn poll_incremental(&mut self) -> Option<(TokenUsage, u64)> {
        if self.transcript_path.is_none() {
            if let (Some(ref bdir), Some(ref cid)) = (&self.brain_dir, &self.conv_id) {
                self.transcript_path = transcript_path_for_conv(bdir, cid);
            }
        }
        let path = self.transcript_path.as_ref()?;
        let Ok(mut file) = File::open(path) else {
            return None;
        };
        let Ok(meta) = file.metadata() else {
            return None;
        };
        let file_len = meta.len();
        if file_len <= self.last_offset {
            return None;
        }
        if file.seek(SeekFrom::Start(self.last_offset)).is_err() {
            return None;
        }

        let mut buf = Vec::with_capacity((file_len - self.last_offset) as usize);
        if file.read_to_end(&mut buf).is_err() || buf.is_empty() {
            return None;
        }

        // 仅消费最后一个 `\n` 之前的完整行，避免读到模型刚写到一半的半行 JSON。
        let complete_len = match buf.iter().rposition(|&b| b == b'\n') {
            Some(idx) => idx + 1,
            None => return None,
        };
        self.last_offset = self.last_offset.saturating_add(complete_len as u64);

        let text = String::from_utf8_lossy(&buf[..complete_len]);
        let mut changed = false;

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || !trimmed.starts_with('{') {
                continue;
            }
            let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if let Some((inp, cache, out, think)) = extract_step_token_quad(&val) {
                self.usage.input_tokens = self.usage.input_tokens.saturating_add(inp);
                self.usage.cache_read_tokens = self.usage.cache_read_tokens.saturating_add(cache);
                self.usage.output_tokens = self.usage.output_tokens.saturating_add(out);
                self.usage.thinking_tokens = self.usage.thinking_tokens.saturating_add(think);
                let step_ctx = inp.saturating_add(cache);
                self.peak_context_tokens = self.peak_context_tokens.max(step_ctx);
                changed = true;
            }
        }

        if changed {
            Some(self.current_snapshot())
        } else {
            None
        }
    }

    /// 返回当前轮次已追踪到的实时累计快照（含 `transcript.jsonl` 与 `step_update` 载荷最大值）。
    /// 注意：`peak_context_tokens` 严格为单步上下文窗口峰值，绝不拿多步累计和冒充。
    pub fn current_snapshot(&self) -> (TokenUsage, u64) {
        let merged = merge_elementwise_max(&self.usage, &self.step_payload_usage);
        (merged, self.peak_context_tokens)
    }

    /// 轮次正常结束收到 `Result` 时做最终权威对齐（逐项取最大值，绝不双倍计费）。
    pub fn reconcile_final_usage(&mut self, result_usage: &TokenUsage) -> (TokenUsage, u64) {
        let _ = self.poll_incremental();
        let (tracked, tracked_peak) = self.current_snapshot();
        let final_usage = merge_elementwise_max(&tracked, result_usage);
        let final_peak = if tracked_peak > 0 {
            tracked_peak
        } else {
            result_usage
                .input_tokens
                .saturating_add(result_usage.cache_read_tokens)
        };
        (final_usage, final_peak)
    }

    pub fn has_any_usage(&self) -> bool {
        let (u, _) = self.current_snapshot();
        u.total_tokens > 0
    }
}

pub fn merge_elementwise_max(a: &TokenUsage, b: &TokenUsage) -> TokenUsage {
    let input_tokens = a.input_tokens.max(b.input_tokens);
    let output_tokens = a.output_tokens.max(b.output_tokens);
    let thinking_tokens = a.thinking_tokens.max(b.thinking_tokens);
    let cache_read_tokens = a.cache_read_tokens.max(b.cache_read_tokens);
    let sum = input_tokens
        .saturating_add(cache_read_tokens)
        .saturating_add(output_tokens)
        .saturating_add(thinking_tokens);
    let total_tokens = sum.max(a.total_tokens).max(b.total_tokens);
    TokenUsage {
        input_tokens,
        output_tokens,
        thinking_tokens,
        cache_read_tokens,
        total_tokens,
    }
}

/// 判断流事件是否属于「实质性模型进展」（用于 45 秒首步响应看门狗）：
/// - `init` 或纯 `checkpoint` 保存检查点不算实质进展；
/// - 只要产出 `thinking`、`tool`、非空文本、`agent_response` 或 `result` 即解除首步看门狗。
pub fn is_substantive_progress_event(evt: &ParsedStreamEvent) -> bool {
    match evt {
        ParsedStreamEvent::Result(_) => true,
        ParsedStreamEvent::Init { .. } | ParsedStreamEvent::Ignored => false,
        ParsedStreamEvent::StepUpdate {
            step_type,
            is_tool_step,
            has_thinking,
            has_text,
            ..
        } => {
            if *is_tool_step || *has_thinking || *has_text {
                return true;
            }
            let st = step_type.trim().to_ascii_lowercase();
            !st.is_empty() && st != "checkpoint" && st != "init" && st != "unknown"
        }
    }
}
