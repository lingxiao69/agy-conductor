use crate::agy::gateway_wildcards::{
    prepare_gateway_session_settings, sanitize_wildcard_model_id,
};
use crate::agy::live_log::{log_file_for_slot, sanitize_slot_filename_id};
use crate::agy::resolver::{find_in_path, ResolvedAgyRuntime};
use crate::agy::terminal_scripts::{
    build_cmd_launcher_content, build_interactive_cli_ps1, build_live_tail_ps1,
};
use crate::orchestrator::types::AgentSlot;
use crate::storage::store::atomic_write_text;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalHost {
    WindowsTerminal(PathBuf),
    Pwsh(PathBuf),
    PowerShell(PathBuf),
    Cmd(PathBuf),
}

impl TerminalHost {
    pub fn display_name(&self) -> String {
        match self {
            Self::WindowsTerminal(_) => "Windows Terminal (wt.exe)".to_string(),
            Self::Pwsh(_) => "PowerShell 7 (pwsh.exe)".to_string(),
            Self::PowerShell(_) => "Windows PowerShell (powershell.exe)".to_string(),
            Self::Cmd(_) => "Command Prompt (cmd.exe)".to_string(),
        }
    }

    pub fn supports_split_pane(&self) -> bool {
        matches!(self, Self::WindowsTerminal(_))
    }
}

pub fn detect_terminal_host() -> TerminalHost {
    if let Some(p) = find_in_path(&["wt.exe"]) {
        return TerminalHost::WindowsTerminal(p);
    }
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let wt = PathBuf::from(local_app_data)
            .join("Microsoft")
            .join("WindowsApps")
            .join("wt.exe");
        if wt.is_file() {
            return TerminalHost::WindowsTerminal(wt);
        }
    }
    if let Some(p) = find_in_path(&["pwsh.exe", "pwsh"]) {
        return TerminalHost::Pwsh(p);
    }
    if let Some(p) = find_in_path(&["powershell.exe"]) {
        return TerminalHost::PowerShell(p);
    }
    TerminalHost::Cmd(PathBuf::from("cmd.exe"))
}

#[derive(Debug, Clone)]
pub struct TerminalLaunchOptions {
    pub workspace_dir: PathBuf,
    pub room_dir: Option<PathBuf>,
    pub mount_mcp: bool,
    pub attach_conversation: bool,
    pub watch_live: bool,
}

#[derive(Debug, Clone)]
pub struct GeneratedTerminalWrapper {
    pub cmd_path: PathBuf,
    pub ps1_path: PathBuf,
    pub ps_exe: PathBuf,
    pub title: String,
    pub log_path: Option<PathBuf>,
}

/// 转义传给 Windows Terminal (`wt.exe`) 单个参数的特殊控制符：
/// - `;` 在 `wt.exe` 中会被视作多窗格子命令分隔符，必须转义为 `\;`；
/// - 清理换行与回车符为空格，防止破坏窗口标题或命令行解析。
pub fn escape_wt_arg(raw: &str) -> String {
    raw.replace(['\r', '\n'], " ").replace(';', "\\;")
}

fn build_mcp_servers_value(room_dir: &Path, alias: &str) -> Result<serde_json::Value, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("无法获取当前可执行文件路径: {e}"))?;
    Ok(json!({
        "agy-conductor-hub": {
            "command": exe.to_string_lossy(),
            "args": [
                "mcp-stdio",
                "--room",
                room_dir.to_string_lossy(),
                "--alias",
                alias
            ]
        }
    }))
}

pub fn build_interactive_agy_command_vec(
    runtime: &ResolvedAgyRuntime,
    slot: Option<&AgentSlot>,
    opts: &TerminalLaunchOptions,
) -> Result<Vec<String>, String> {
    let (program, mut args) = runtime.program_and_prefix();
    let mut full = Vec::new();
    full.push(program.to_string_lossy().to_string());
    full.append(&mut args);

    let sanitized_model = slot
        .map(|s| sanitize_wildcard_model_id(&s.model_id))
        .unwrap_or_default();

    if let Some(slot) = slot {
        if !sanitized_model.trim().is_empty() {
            full.push("--model".to_string());
            full.push(sanitized_model.clone());
        }
        if opts.attach_conversation {
            if let Some(conv_id) = slot
                .conversation_id
                .as_deref()
                .filter(|s| !s.trim().is_empty())
            {
                full.push("--conversation".to_string());
                full.push(conv_id.to_string());
            }
        }
    }

    if !opts.watch_live {
        let extra_mcp = if opts.mount_mcp {
            if let Some(room_dir) = &opts.room_dir {
                let alias = slot.map(|s| s.alias.as_str()).unwrap_or("cli-user");
                Some(build_mcp_servers_value(room_dir, alias)?)
            } else {
                None
            }
        } else {
            None
        };
        let settings_dir = opts
            .room_dir
            .clone()
            .unwrap_or_else(|| resolve_launcher_dir(None));
        let stem = slot
            .map(|s| sanitize_slot_filename_id(&s.slot_id))
            .unwrap_or_else(|| "cli-user".to_string());
        if let Some(settings_path) = prepare_gateway_session_settings(
            &sanitized_model,
            &settings_dir,
            &stem,
            extra_mcp,
            None,
        )? {
            full.push("--settings".to_string());
            full.push(settings_path.to_string_lossy().to_string());
        }
    }

    Ok(full)
}

fn resolve_launcher_dir(room_dir: Option<&Path>) -> PathBuf {
    if let Some(rd) = room_dir {
        rd.join("launchers")
    } else {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("AgyConductor")
            .join("launchers")
    }
}

/// 生成 UTF-8 无 BOM 的 `.ps1` + `.cmd` 包装脚本，彻底避免向 `wt.exe` 裸传 `--model` / `--settings` 冲突
/// 并于启动第 1 毫秒立即展示彩色状态横幅消灭黑屏。
pub fn prepare_terminal_wrapper(
    runtime: &ResolvedAgyRuntime,
    slot: Option<&AgentSlot>,
    opts: &TerminalLaunchOptions,
    workdir: &Path,
) -> Result<GeneratedTerminalWrapper, String> {
    let launcher_dir = resolve_launcher_dir(opts.room_dir.as_deref());
    fs::create_dir_all(&launcher_dir)
        .map_err(|e| format!("创建终端脚本目录失败 {}: {e}", launcher_dir.display()))?;

    let slot_tag = slot
        .map(|s| sanitize_slot_filename_id(&s.slot_id))
        .unwrap_or_else(|| "all".to_string());
    let mode_tag = if opts.watch_live {
        "live"
    } else if opts.attach_conversation {
        "attach"
    } else {
        "cli"
    };

    let title = match slot {
        Some(s) if opts.watch_live => format!("实时追踪 · {} (@{})", s.display_name, s.alias),
        Some(s) if opts.attach_conversation && s.conversation_id.is_some() => {
            format!("接管 · {} (@{})", s.display_name, s.alias)
        }
        Some(s) => format!("{} · P{}", s.display_name, s.priority),
        None if opts.watch_live => "轻舟实时 AI 进程总流".to_string(),
        None => "轻舟 AgyConductor CLI".to_string(),
    };

    let ps1_path = launcher_dir.join(format!("launch-{mode_tag}-{slot_tag}.ps1"));
    let cmd_path = launcher_dir.join(format!("launch-{mode_tag}-{slot_tag}.cmd"));

    let (ps1_content, log_path) = if opts.watch_live {
        let base_room = opts.room_dir.clone().unwrap_or_else(|| launcher_dir.clone());
        let log_file = log_file_for_slot(&base_room, slot.map(|s| s.slot_id.as_str()));
        if let Some(parent) = log_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if !log_file.exists() {
            let _ = OpenOptions::new().create(true).append(true).open(&log_file);
        }
        let script = build_live_tail_ps1(&title, slot, workdir, &log_file);
        (script, Some(log_file))
    } else {
        let agy_cmd = build_interactive_agy_command_vec(runtime, slot, opts)?;
        let script = build_interactive_cli_ps1(&title, slot, workdir, &agy_cmd, opts);
        (script, None)
    };

    atomic_write_text(&ps1_path, &ps1_content)?;

    let ps_exe = find_in_path(&["pwsh.exe", "pwsh"])
        .unwrap_or_else(|| PathBuf::from("powershell.exe"));
    let cmd_content = build_cmd_launcher_content(&ps_exe, &ps1_path);
    atomic_write_text(&cmd_path, &cmd_content)?;

    Ok(GeneratedTerminalWrapper {
        cmd_path,
        ps1_path,
        ps_exe,
        title,
        log_path,
    })
}

/// 构造传给 `wt.exe` 的安全参数序列：
/// - 绝不向 `wt.exe` 直接裸传 `--model` / `--settings`；
/// - 统一转义参数中的 `;` 为 `\;` 并直接调用 PowerShell `-File`，彻底绕开 `cmd.exe /K` 对 `&` / `|` / `()` 的二次解析。
pub fn build_wt_args_for_wrappers(
    wrappers: &[GeneratedTerminalWrapper],
    workdir: &Path,
) -> Vec<String> {
    let mut wt_args = Vec::new();
    let wd = escape_wt_arg(&workdir.to_string_lossy());
    for (idx, w) in wrappers.iter().enumerate() {
        if idx == 0 {
            wt_args.push("new-tab".to_string());
        } else {
            wt_args.push(";".to_string());
            wt_args.push("split-pane".to_string());
            wt_args.push(if idx % 2 == 1 { "-V" } else { "-H" }.to_string());
        }
        wt_args.push("--title".to_string());
        wt_args.push(escape_wt_arg(&w.title));
        wt_args.push("-d".to_string());
        wt_args.push(wd.clone());
        wt_args.push(escape_wt_arg(&w.ps_exe.to_string_lossy()));
        wt_args.push("-NoLogo".to_string());
        wt_args.push("-NoExit".to_string());
        wt_args.push("-ExecutionPolicy".to_string());
        wt_args.push("Bypass".to_string());
        wt_args.push("-File".to_string());
        wt_args.push(escape_wt_arg(&w.ps1_path.to_string_lossy()));
    }
    wt_args
}

/// Launches a single external terminal (interactive CLI or live tail).
/// Intentionally NOT assigned to `ProcessJobGuard`.
pub fn launch_single_cli(
    runtime: &ResolvedAgyRuntime,
    slot: Option<&AgentSlot>,
    opts: &TerminalLaunchOptions,
) -> Result<String, String> {
    let host = detect_terminal_host();
    let workdir = resolve_workdir(&opts.workspace_dir);
    let wrapper = prepare_terminal_wrapper(runtime, slot, opts, &workdir)?;
    spawn_wrapper_on_host(&host, &workdir, &wrapper)?;
    Ok(format!(
        "已在 {} 中打开「{}」",
        host.display_name(),
        wrapper.title
    ))
}

/// Launches a split-pane team terminal in Windows Terminal (`wt.exe`),
/// or falls back to individual windows on other terminal hosts.
pub fn launch_split_team_cli(
    runtime: &ResolvedAgyRuntime,
    slots: &[AgentSlot],
    opts: &TerminalLaunchOptions,
) -> Result<String, String> {
    let active_slots: Vec<&AgentSlot> = slots.iter().filter(|s| s.enabled).take(4).collect();
    if active_slots.is_empty() {
        return launch_single_cli(runtime, None, opts);
    }

    let workdir = resolve_workdir(&opts.workspace_dir);
    let mut wrappers = Vec::with_capacity(active_slots.len());
    for slot in &active_slots {
        wrappers.push(prepare_terminal_wrapper(runtime, Some(slot), opts, &workdir)?);
    }

    let host = detect_terminal_host();
    if let TerminalHost::WindowsTerminal(wt_path) = host {
        let wt_args = build_wt_args_for_wrappers(&wrappers, &workdir);
        Command::new(wt_path)
            .args(&wt_args)
            .spawn()
            .map_err(|e| format!("拉起 Windows Terminal 多分屏失败: {e}"))?;
        let label = if opts.watch_live {
            "实时进程追踪分屏"
        } else {
            "编队交互式 CLI 分屏"
        };
        return Ok(format!(
            "已在 Windows Terminal 拉起 {} 个{label}",
            wrappers.len()
        ));
    }

    for wrapper in &wrappers {
        spawn_wrapper_on_host(&host, &workdir, wrapper)?;
    }

    Ok(format!(
        "已在 {} 中打开 {} 个编队终端窗口",
        host.display_name(),
        wrappers.len()
    ))
}

fn spawn_wrapper_on_host(
    host: &TerminalHost,
    workdir: &Path,
    wrapper: &GeneratedTerminalWrapper,
) -> Result<(), String> {
    match host {
        TerminalHost::WindowsTerminal(wt_path) => {
            let wt_args = build_wt_args_for_wrappers(std::slice::from_ref(wrapper), workdir);
            Command::new(wt_path)
                .args(&wt_args)
                .spawn()
                .map_err(|e| format!("拉起 Windows Terminal 失败: {e}"))?;
        }
        TerminalHost::Pwsh(_) | TerminalHost::PowerShell(_) | TerminalHost::Cmd(_) => {
            let mut cmd = Command::new(&wrapper.ps_exe);
            cmd.current_dir(workdir).args([
                "-NoLogo",
                "-NoExit",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                &wrapper.ps1_path.to_string_lossy(),
            ]);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
                cmd.creation_flags(CREATE_NEW_CONSOLE);
            }
            cmd.spawn()
                .map_err(|e| format!("拉起外部终端失败: {e}"))?;
        }
    }
    Ok(())
}

fn resolve_workdir(workspace_dir: &Path) -> PathBuf {
    if workspace_dir.is_dir() {
        workspace_dir.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

pub fn open_folder_in_explorer(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("目录不存在: {}", path.display()));
    }
    #[cfg(windows)]
    {
        Command::new("explorer.exe")
            .arg(path)
            .spawn()
            .map_err(|e| format!("打开资源管理器失败: {e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("当前平台暂不支持打开资源管理器".to_string())
    }
}
