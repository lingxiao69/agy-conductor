use serde::{Deserialize, Serialize};
use std::env;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgyLaunchStrategy {
    /// Detected `gateway-bin/agy.cmd` with sibling `../gateway-support/launch.mjs`.
    /// Invoke `node.exe launch.mjs ...` directly to avoid batch `%*` argument corruption.
    GatewayNodeWrapper {
        node_exe: PathBuf,
        launch_mjs: PathBuf,
        underlying_agy_exe: Option<PathBuf>,
        cmd_script: PathBuf,
    },
    /// Standard official binary (`agy.exe` or `agy`).
    DirectBinary { agy_exe: PathBuf },
    /// Fallback custom `.cmd` / `.bat` script when `launch.mjs` is not present.
    GenericBatch { cmd_script: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedAgyRuntime {
    pub strategy: AgyLaunchStrategy,
    pub display_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub agy_found: bool,
    pub runtime_kind: String,
    pub runtime_path: String,
    pub version: String,
    pub node_available: bool,
    pub terminal_host: String,
    pub gateway_mode: bool,
    pub notes: Vec<String>,
}

impl ResolvedAgyRuntime {
    pub fn runtime_kind_label(&self) -> String {
        match &self.strategy {
            AgyLaunchStrategy::GatewayNodeWrapper { .. } => "Gateway 启动器 (launch.mjs)".to_string(),
            AgyLaunchStrategy::DirectBinary { .. } => "官方直连 (agy.exe)".to_string(),
            AgyLaunchStrategy::GenericBatch { .. } => "批处理包装器 (agy.cmd)".to_string(),
        }
    }

    pub fn is_gateway_wrapper(&self) -> bool {
        matches!(self.strategy, AgyLaunchStrategy::GatewayNodeWrapper { .. })
    }

    /// Build program path and prefix arguments for executing `agy` with zero shell quoting issues.
    pub fn program_and_prefix(&self) -> (PathBuf, Vec<String>) {
        match &self.strategy {
            AgyLaunchStrategy::GatewayNodeWrapper {
                node_exe,
                launch_mjs,
                ..
            } => (
                node_exe.clone(),
                vec![launch_mjs.to_string_lossy().to_string()],
            ),
            AgyLaunchStrategy::DirectBinary { agy_exe } => (agy_exe.clone(), Vec::new()),
            AgyLaunchStrategy::GenericBatch { cmd_script } => (
                PathBuf::from("cmd.exe"),
                vec![
                    "/D".to_string(),
                    "/C".to_string(),
                    cmd_script.to_string_lossy().to_string(),
                ],
            ),
        }
    }

    /// Build a Tokio command for headless background invocation with `CREATE_NO_WINDOW`.
    pub fn build_headless_command(&self, extra_args: &[String]) -> Command {
        let (program, prefix) = self.program_and_prefix();
        let mut cmd = Command::new(program);
        cmd.args(prefix);
        cmd.args(extra_args);
        #[cfg(windows)]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }

    /// Build the update command. For `GatewayNodeWrapper`, invokes `underlying_agy_exe update`
    /// directly when present so the official binary updates without disturbing gateway scripts.
    pub fn build_update_command(&self) -> Command {
        let mut cmd = match &self.strategy {
            AgyLaunchStrategy::GatewayNodeWrapper {
                underlying_agy_exe: Some(exe),
                ..
            } => {
                let mut c = Command::new(exe);
                c.arg("update");
                c
            }
            _ => self.build_headless_command(&["update".to_string()]),
        };
        #[cfg(windows)]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    }
}

pub fn resolve_agy_runtime(custom_path: Option<&str>) -> Result<ResolvedAgyRuntime, String> {
    if let Some(raw) = custom_path.map(str::trim).filter(|s| !s.is_empty()) {
        let candidate = PathBuf::from(raw);
        if candidate.is_file() {
            return classify_candidate(&candidate);
        }
        return Err(format!("自定义 agy 路径不存在: {}", candidate.display()));
    }

    for candidate in discover_agy_candidates() {
        if candidate.is_file() {
            return classify_candidate(&candidate);
        }
    }

    Err("未在 PATH 或默认安装目录找到 agy，请确认已安装 Antigravity CLI。".to_string())
}

fn discover_agy_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let exts = pathext_candidates();

    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            for ext in &exts {
                out.push(dir.join(format!("agy{ext}")));
            }
            #[cfg(not(windows))]
            out.push(dir.join("agy"));
        }
    }

    if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
        let base = PathBuf::from(local_app_data).join("agy");
        out.push(base.join("gateway-bin").join("agy.cmd"));
        out.push(base.join("bin").join("agy.exe"));
    }

    out
}

fn pathext_candidates() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            ".cmd".to_string(),
            ".exe".to_string(),
            ".bat".to_string(),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![String::new()]
    }
}

pub fn find_in_path(binary_names: &[&str]) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        for name in binary_names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn classify_candidate(candidate: &Path) -> Result<ResolvedAgyRuntime, String> {
    let ext = candidate
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if ext == "cmd" || ext == "bat" {
        if let Some(launch_mjs) = find_gateway_launch_mjs(candidate) {
            if let Some(node_exe) = find_in_path(&["node.exe", "node"]) {
                let underlying_agy_exe = candidate
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|p| p.join("bin").join("agy.exe"))
                    .filter(|p| p.is_file());
                return Ok(ResolvedAgyRuntime {
                    display_path: candidate.to_path_buf(),
                    strategy: AgyLaunchStrategy::GatewayNodeWrapper {
                        node_exe,
                        launch_mjs,
                        underlying_agy_exe,
                        cmd_script: candidate.to_path_buf(),
                    },
                });
            }
        }
        return Ok(ResolvedAgyRuntime {
            display_path: candidate.to_path_buf(),
            strategy: AgyLaunchStrategy::GenericBatch {
                cmd_script: candidate.to_path_buf(),
            },
        });
    }

    Ok(ResolvedAgyRuntime {
        display_path: candidate.to_path_buf(),
        strategy: AgyLaunchStrategy::DirectBinary {
            agy_exe: candidate.to_path_buf(),
        },
    })
}

fn find_gateway_launch_mjs(cmd_path: &Path) -> Option<PathBuf> {
    let parent = cmd_path.parent()?;
    let root = parent.parent()?;
    let launch_mjs = root.join("gateway-support").join("launch.mjs");
    if launch_mjs.is_file() {
        Some(launch_mjs)
    } else {
        None
    }
}

pub async fn probe_agy_version(runtime: &ResolvedAgyRuntime) -> Result<String, String> {
    let mut cmd = runtime.build_headless_command(&["--version".to_string()]);
    let output = timeout(Duration::from_secs(10), cmd.output())
        .await
        .map_err(|_| "探测 agy 版本超时 (10s)".to_string())?
        .map_err(|e| format!("执行 agy --version 失败: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "agy --version 返回非零状态 ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        "unknown".to_string()
    } else {
        stdout
    })
}

/// Reads `~/.gemini/antigravity-cli/settings.json` strictly for the model source preference
/// without retaining or logging any credentials.
pub fn detect_user_prefers_gateway() -> bool {
    let Some(home) = dirs::home_dir() else {
        return true;
    };
    let settings_path = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("settings.json");
    let Ok(raw) = std::fs::read_to_string(settings_path) else {
        return true;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return true;
    };
    if let Some(model) = val.get("model").and_then(|v| v.as_str()) {
        return model.trim_start().starts_with("[Gateway]");
    }
    true
}

pub async fn run_doctor(custom_path: Option<&str>) -> DoctorReport {
    let node_available = find_in_path(&["node.exe", "node"]).is_some();
    let terminal_host = crate::agy::terminal::detect_terminal_host().display_name();
    let mut notes = Vec::new();

    match resolve_agy_runtime(custom_path) {
        Ok(rt) => {
            let version = match probe_agy_version(&rt).await {
                Ok(v) => v,
                Err(e) => {
                    notes.push(e);
                    "不可用".to_string()
                }
            };
            if rt.is_gateway_wrapper() {
                notes.push("已启用 launch.mjs 零转义直连模式，[Gateway] 模型与 -xhigh 代理受完整支持。".to_string());
            }
            DoctorReport {
                agy_found: true,
                runtime_kind: rt.runtime_kind_label(),
                runtime_path: rt.display_path.to_string_lossy().to_string(),
                version,
                node_available,
                terminal_host,
                gateway_mode: rt.is_gateway_wrapper(),
                notes,
            }
        }
        Err(err) => {
            notes.push(err);
            DoctorReport {
                agy_found: false,
                runtime_kind: "未检测到".to_string(),
                runtime_path: String::new(),
                version: "-".to_string(),
                node_available,
                terminal_host,
                gateway_mode: false,
                notes,
            }
        }
    }
}
