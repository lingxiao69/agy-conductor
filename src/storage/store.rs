use crate::billing::SlotUsageStats;
use crate::cli_hub::CliProviderId;
use crate::orchestrator::types::{
    AgentSlot, GroupChatMessage, PermissionMode, SameRoleDispatchMode, TaskItem,
};
use crate::storage::presets::RosterPreset;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedConfig {
    pub custom_agy_path: Option<String>,
    pub prefer_gateway_models: bool,
    pub last_workspace: String,
    pub dispatch_mode: SameRoleDispatchMode,
    pub permission_mode: PermissionMode,
    pub enable_sandbox: bool,
    pub max_rework_rounds: u8,
    pub slots: Vec<AgentSlot>,
    #[serde(default)]
    pub presets: Vec<RosterPreset>,
    #[serde(default)]
    pub active_preset_id: Option<String>,
    #[serde(default = "default_reuse_sessions")]
    pub reuse_role_sessions: bool,
    #[serde(default)]
    pub active_cli_provider: CliProviderId,
}

fn default_reuse_sessions() -> bool {
    true
}

impl Default for PersistedConfig {
    fn default() -> Self {
        Self {
            custom_agy_path: None,
            prefer_gateway_models: true,
            last_workspace: String::new(),
            dispatch_mode: SameRoleDispatchMode::PriorityWithOverflow,
            permission_mode: PermissionMode::AcceptEdits,
            enable_sandbox: false,
            max_rework_rounds: 8,
            slots: Vec::new(),
            presets: Vec::new(),
            active_preset_id: None,
            reuse_role_sessions: true,
            active_cli_provider: CliProviderId::Agy,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub room_id: String,
    pub workspace: String,
    pub goal: String,
    pub phase: String,
    pub tasks: Vec<TaskItem>,
    pub updated_at: String,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub slot_usages: Vec<(String, SlotUsageStats)>,
}

#[derive(Debug, Clone)]
pub struct AppStorage {
    base_dir: PathBuf,
}

impl AppStorage {
    pub fn default_location() -> Self {
        let base = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AgyConductor");
        Self { base_dir: base }
    }

    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn config_path(&self) -> PathBuf {
        self.base_dir.join("config.json")
    }

    pub fn schemas_dir(&self) -> PathBuf {
        self.base_dir.join("schemas")
    }

    pub fn rooms_dir(&self) -> PathBuf {
        self.base_dir.join("rooms")
    }

    pub fn ensure_dirs(&self) -> Result<(), String> {
        fs::create_dir_all(&self.base_dir)
            .map_err(|e| format!("创建数据目录失败 {}: {e}", self.base_dir.display()))?;
        fs::create_dir_all(self.schemas_dir())
            .map_err(|e| format!("创建 Schema 目录失败: {e}"))?;
        fs::create_dir_all(self.rooms_dir())
            .map_err(|e| format!("创建群组目录失败: {e}"))?;
        Ok(())
    }

    pub fn load_config(&self) -> PersistedConfig {
        let path = self.config_path();
        match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => PersistedConfig::default(),
        }
    }

    pub fn save_config(&self, config: &PersistedConfig) -> Result<(), String> {
        self.ensure_dirs()?;
        let path = self.config_path();
        let json = serde_json::to_string_pretty(config)
            .map_err(|e| format!("序列化配置失败: {e}"))?;
        atomic_write_text(&path, &json)
    }

    pub fn create_room_dir(&self, room_id: &str) -> Result<PathBuf, String> {
        self.ensure_dirs()?;
        let dir = self.rooms_dir().join(room_id);
        fs::create_dir_all(&dir)
            .map_err(|e| format!("创建群组目录失败 {}: {e}", dir.display()))?;
        Ok(dir)
    }

    pub fn append_board_message(
        room_dir: &Path,
        message: &GroupChatMessage,
    ) -> Result<(), String> {
        fs::create_dir_all(room_dir)
            .map_err(|e| format!("确保群组目录存在失败: {e}"))?;
        let path = room_dir.join("board.jsonl");
        let mut buf = serde_json::to_vec(message)
            .map_err(|e| format!("序列化群消息失败: {e}"))?;
        buf.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("打开 board.jsonl 失败: {e}"))?;
        file.write_all(&buf)
            .map_err(|e| format!("写入 board.jsonl 失败: {e}"))?;
        file.flush().map_err(|e| format!("刷新 board.jsonl 失败: {e}"))
    }

    pub fn read_board_messages(room_dir: &Path) -> Result<Vec<GroupChatMessage>, String> {
        let path = room_dir.join("board.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&path)
            .map_err(|e| format!("读取 board.jsonl 失败: {e}"))?;
        let mut reader = BufReader::new(file);
        let mut out = Vec::new();
        let mut raw = Vec::new();
        loop {
            raw.clear();
            let n = reader
                .read_until(b'\n', &mut raw)
                .map_err(|e| format!("逐行读取 board.jsonl 失败: {e}"))?;
            if n == 0 {
                break;
            }
            let decoded = String::from_utf8_lossy(&raw);
            let trimmed = decoded.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(msg) = serde_json::from_str::<GroupChatMessage>(trimmed) {
                out.push(msg);
            }
        }
        Ok(out)
    }

    pub fn save_room_snapshot(room_dir: &Path, snapshot: &RoomSnapshot) -> Result<(), String> {
        fs::create_dir_all(room_dir)
            .map_err(|e| format!("确保群组目录存在失败: {e}"))?;
        let path = room_dir.join("state.json");
        let json = serde_json::to_string_pretty(snapshot)
            .map_err(|e| format!("序列化状态快照失败: {e}"))?;
        atomic_write_text(&path, &json)
    }

    pub fn read_room_snapshot(room_dir: &Path) -> Option<RoomSnapshot> {
        let path = room_dir.join("state.json");
        let raw = fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }
}

pub fn atomic_write_text(target: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("创建父目录失败 {}: {e}", parent.display()))?;
    }
    let tmp_name = format!(
        ".{}.{}.tmp",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file"),
        uuid::Uuid::new_v4()
    );
    let tmp_path = target.with_file_name(tmp_name);
    fs::write(&tmp_path, content.as_bytes())
        .map_err(|e| format!("写入临时文件失败 {}: {e}", tmp_path.display()))?;

    let replace_res = replace_file_atomically(&tmp_path, target);
    if replace_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    replace_res
}

#[cfg(windows)]
fn replace_file_atomically(tmp_path: &Path, target: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use std::time::Duration;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let src: Vec<u16> = tmp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut last_err = std::io::Error::other("MoveFileExW failed");
    for attempt in 0..3 {
        let ok = unsafe {
            MoveFileExW(
                src.as_ptr(),
                dst.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        last_err = std::io::Error::last_os_error();
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(15));
        }
    }
    Err(format!("原子替换文件失败 {}: {last_err}", target.display()))
}

#[cfg(not(windows))]
fn replace_file_atomically(tmp_path: &Path, target: &Path) -> Result<(), String> {
    fs::rename(tmp_path, target)
        .map_err(|e| format!("原子替换文件失败 {}: {e}", target.display()))
}
