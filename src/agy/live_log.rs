use crate::orchestrator::types::AgentSlot;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveStreamKind {
    System,
    Thinking,
    ToolCall,
    Output,
    Stderr,
    Result,
}

impl LiveStreamKind {
    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::System => "系统",
            Self::Thinking => "思考",
            Self::ToolCall => "工具",
            Self::Output => "输出",
            Self::Stderr => "错误",
            Self::Result => "结果",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveTerminalLine {
    pub seq: u64,
    pub timestamp: String,
    pub slot_id: String,
    pub alias: String,
    pub display_name: String,
    pub kind: LiveStreamKind,
    pub text: String,
}

/// 线程安全的实时 AI 进程日志汇聚器：
/// 同时向内存 `mpsc` 通道（供内置暗色终端渲染）与房间目录 UTF-8 无 BOM 日志文件追加实时行。
pub struct LiveLogSink {
    room_dir: PathBuf,
    ui_tx: Option<mpsc::UnboundedSender<LiveTerminalLine>>,
    seq: AtomicU64,
    file_lock: Mutex<()>,
    slot_meta: RwLock<HashMap<String, (String, String)>>,
}

impl LiveLogSink {
    pub fn new(
        room_dir: PathBuf,
        ui_tx: Option<mpsc::UnboundedSender<LiveTerminalLine>>,
        slots: &[AgentSlot],
    ) -> Self {
        let mut meta = HashMap::new();
        for s in slots {
            meta.insert(
                s.slot_id.clone(),
                (s.alias.clone(), s.display_name.clone()),
            );
        }
        let sink = Self {
            room_dir,
            ui_tx,
            seq: AtomicU64::new(1),
            file_lock: Mutex::new(()),
            slot_meta: RwLock::new(meta),
        };
        sink.ensure_log_files(slots);
        sink
    }

    pub fn room_dir(&self) -> &Path {
        &self.room_dir
    }

    pub fn sync_slots(&self, slots: &[AgentSlot]) {
        if let Ok(mut guard) = self.slot_meta.write() {
            for s in slots {
                guard.insert(
                    s.slot_id.clone(),
                    (s.alias.clone(), s.display_name.clone()),
                );
            }
        }
        self.ensure_log_files(slots);
    }

    pub fn ensure_log_files(&self, slots: &[AgentSlot]) {
        let _guard = self.file_lock.lock().ok();
        let _ = fs::create_dir_all(&self.room_dir);
        let all_path = log_file_for_slot(&self.room_dir, None);
        if !all_path.exists() {
            let _ = OpenOptions::new().create(true).append(true).open(&all_path);
        }
        for s in slots {
            let p = log_file_for_slot(&self.room_dir, Some(&s.slot_id));
            if !p.exists() {
                let _ = OpenOptions::new().create(true).append(true).open(&p);
            }
        }
    }

    pub fn append_for_slot(
        &self,
        slot_id: &str,
        kind: LiveStreamKind,
        text: impl Into<String>,
    ) -> LiveTerminalLine {
        let (alias, display_name) = self
            .slot_meta
            .read()
            .ok()
            .and_then(|m| m.get(slot_id).cloned())
            .unwrap_or_else(|| (slot_id.to_string(), slot_id.to_string()));
        self.append_line(slot_id, &alias, &display_name, kind, text)
    }

    pub fn append_line(
        &self,
        slot_id: &str,
        alias: &str,
        display_name: &str,
        kind: LiveStreamKind,
        text: impl Into<String>,
    ) -> LiveTerminalLine {
        let clean_text = text.into().replace('\r', "");
        let line = LiveTerminalLine {
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            timestamp: Local::now().format("%H:%M:%S").to_string(),
            slot_id: slot_id.to_string(),
            alias: alias.to_string(),
            display_name: display_name.to_string(),
            kind,
            text: clean_text,
        };

        let formatted = format_log_file_line(&line);
        if let Ok(_guard) = self.file_lock.lock() {
            let _ = fs::create_dir_all(&self.room_dir);
            let all_path = log_file_for_slot(&self.room_dir, None);
            let _ = append_utf8_line(&all_path, &formatted);
            if !slot_id.trim().is_empty() && slot_id != "system" {
                let slot_path = log_file_for_slot(&self.room_dir, Some(slot_id));
                let _ = append_utf8_line(&slot_path, &formatted);
            }
        }

        if let Some(tx) = &self.ui_tx {
            let _ = tx.send(line.clone());
        }
        line
    }
}

pub fn sanitize_slot_filename_id(slot_id: &str) -> String {
    let clean: String = slot_id
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "slot".to_string()
    } else {
        clean
    }
}

pub fn log_file_for_slot(room_dir: &Path, slot_id: Option<&str>) -> PathBuf {
    match slot_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => room_dir.join(format!("live-{}.log", sanitize_slot_filename_id(id))),
        None => room_dir.join("live-all.log"),
    }
}

fn format_log_file_line(line: &LiveTerminalLine) -> String {
    let single_line = line
        .text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    format!(
        "[{}] [@{}] [{}] {}",
        line.timestamp,
        line.alias,
        line.kind.label_zh(),
        single_line
    )
}

fn append_utf8_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")?;
    file.flush()
}
