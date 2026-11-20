use crate::agy::resolver::{find_in_path, ResolvedAgyRuntime};
use crate::agy::terminal::{
    detect_terminal_host, launch_single_cli, TerminalHost, TerminalLaunchOptions,
};
use crate::agy::terminal_scripts::{b64_utf8, build_cmd_launcher_content};
use crate::cli_hub::{CliProviderId, CliProviderStatus, CliSupportTier};
use crate::storage::store::atomic_write_text;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::time::Duration;
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

const VERSION_PROBE_TIMEOUT_MS: u64 = 1500;

/// 生成初始占位列表（UI 首帧即时渲染，不阻塞窗口启动）。
pub fn default_cli_providers() -> Vec<CliProviderStatus> {
    CliProviderId::all()
        .into_iter()
        .map(|id| CliProviderStatus {
            id,
            display_name: id.display_name().to_string(),
            short_label: id.short_label().to_string(),
            command_name: id.command_name().to_string(),
            installed: id == CliProviderId::Agy,
            executable_path: None,
            version_hint: None,
            support_tier: if id == CliProviderId::Agy {
                CliSupportTier::ActiveOrchestrator
            } else {
                CliSupportTier::Planned
            },
            tagline: id.tagline().to_string(),
        })
        .collect()
}

/// 并发无副作用探测本机全部内置 AI CLI 安装状态（带 1500ms 超时与 `CREATE_NO_WINDOW` 防闪黑框）。
pub async fn detect_all_cli_providers(
    agy_runtime: Option<&ResolvedAgyRuntime>,
    agy_version: Option<&str>,
) -> Vec<CliProviderStatus> {
    let agy_status = build_agy_status(agy_runtime, agy_version);
    let (codex_status, claude_status, opencode_status) = tokio::join!(
        probe_external_cli(CliProviderId::Codex, &["codex.exe", "codex.cmd", "codex"]),
        probe_external_cli(
            CliProviderId::ClaudeCode,
            &["claude.exe", "claude.cmd", "claude"]
        ),
        probe_external_cli(
            CliProviderId::OpenCode,
            &["opencode.exe", "opencode.cmd", "opencode"]
        ),
    );
    vec![agy_status, codex_status, claude_status, opencode_status]
}

fn build_agy_status(
    agy_runtime: Option<&ResolvedAgyRuntime>,
    agy_version: Option<&str>,
) -> CliProviderStatus {
    let exe_path = agy_runtime
        .map(|rt| rt.display_path.to_string_lossy().to_string())
        .or_else(|| {
            find_cli_binary(&["agy.cmd", "agy.exe", "agy"])
                .map(|p| p.to_string_lossy().to_string())
        });
    let installed = exe_path.is_some();
    let version = agy_version
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "检测中..." && *s != "unknown")
        .map(ToString::to_string);

    CliProviderStatus {
        id: CliProviderId::Agy,
        display_name: CliProviderId::Agy.display_name().to_string(),
        short_label: CliProviderId::Agy.short_label().to_string(),
        command_name: CliProviderId::Agy.command_name().to_string(),
        installed,
        executable_path: exe_path,
        version_hint: version,
        support_tier: CliSupportTier::ActiveOrchestrator,
        tagline: CliProviderId::Agy.tagline().to_string(),
    }
}

async fn probe_external_cli(id: CliProviderId, candidates: &[&str]) -> CliProviderStatus {
    let found_path = find_cli_binary(candidates);
    let installed = found_path.is_some();
    let version_hint = match &found_path {
        Some(path) => probe_cli_version(path).await,
        None => None,
    };

    CliProviderStatus {
        id,
        display_name: id.display_name().to_string(),
        short_label: id.short_label().to_string(),
        command_name: id.command_name().to_string(),
        installed,
        executable_path: found_path.map(|p| p.to_string_lossy().to_string()),
        version_hint,
        support_tier: if installed {
            CliSupportTier::PreviewDetected
        } else {
            CliSupportTier::Planned
        },
        tagline: id.tagline().to_string(),
    }
}

/// 在 `PATH` 及常见用户目录（`%APPDATA%\npm`、`%USERPROFILE%\.local\bin`、`%USERPROFILE%\.cargo\bin` 等）查找命令。
pub fn find_cli_binary(candidates: &[&str]) -> Option<PathBuf> {
    if let Some(found) = find_in_path(candidates) {
        return Some(found);
    }

    let mut search_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        search_dirs.push(PathBuf::from(appdata).join("npm"));
    }
    if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
        let local = PathBuf::from(local_app);
        search_dirs.push(local.join("Microsoft").join("WinGet").join("Links"));
        search_dirs.push(local.join("Programs"));
    }
    if let Some(home) = dirs::home_dir() {
        search_dirs.push(home.join(".local").join("bin"));
        search_dirs.push(home.join(".cargo").join("bin"));
        search_dirs.push(home.join(".opencode").join("bin"));
        search_dirs.push(home.join(".claude").join("local"));
        search_dirs.push(home.join("AppData").join("Roaming").join("npm"));
    }

    for dir in search_dirs {
        if !dir.is_dir() {
            continue;
        }
        for name in candidates {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

async fn probe_cli_version(bin_path: &Path) -> Option<String> {
    let ext = bin_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let mut cmd = if matches!(ext.as_str(), "cmd" | "bat") {
        let mut c = TokioCommand::new("cmd.exe");
        c.args(["/D", "/C", &bin_path.to_string_lossy(), "--version"]);
        c
    } else {
        let mut c = TokioCommand::new(bin_path);
        c.arg("--version");
        c
    };

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let out = timeout(Duration::from_millis(VERSION_PROBE_TIMEOUT_MS), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let raw = if !stdout.trim().is_empty() {
        stdout.as_ref()
    } else {
        stderr.as_ref()
    };
    let first_line = raw.lines().find(|l| !l.trim().is_empty())?.trim();
    Some(first_line.chars().take(64).collect())
}

/// 在当前项目目录拉起指定 AI CLI 的独立外部交互终端窗口（复用无黑屏 UTF-8 `.ps1` + `.cmd` 包装器）。
pub fn launch_provider_external_cli(
    provider: CliProviderId,
    status: &CliProviderStatus,
    workspace: &Path,
    agy_runtime: Option<&ResolvedAgyRuntime>,
    room_dir: Option<&Path>,
) -> Result<String, String> {
    let workdir = if workspace.is_dir() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };

    if provider == CliProviderId::Agy {
        let Some(rt) = agy_runtime else {
            return Err("尚未检测到 agy 运行时，请先点击刷新。".to_string());
        };
        let opts = TerminalLaunchOptions {
            workspace_dir: workdir,
            room_dir: room_dir.map(Path::to_path_buf),
            mount_mcp: false,
            attach_conversation: false,
            watch_live: false,
        };
        return launch_single_cli(rt, None, &opts);
    }

    let Some(exe_path) = status
        .executable_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(format!(
            "本机尚未检测到 {} ({})，请先安装并确认其在 PATH 中后重试。",
            status.display_name, status.command_name
        ));
    };

    let launcher_dir = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("AgyConductor")
        .join("launchers");
    fs::create_dir_all(&launcher_dir)
        .map_err(|e| format!("创建终端脚本目录失败 {}: {e}", launcher_dir.display()))?;

    let tag = status.command_name.as_str();
    let ps1_path = launcher_dir.join(format!("launch-provider-{tag}.ps1"));
    let cmd_path = launcher_dir.join(format!("launch-provider-{tag}.cmd"));
    let title = format!("轻舟 Multi-CLI · {}", status.display_name);

    let ps1_content = format!(
        r#"[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
function _d([string]$b) {{ [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String($b)) }}
$Host.UI.RawUI.WindowTitle = _d '{}'
Write-Host ""
Write-Host "==================================================================" -ForegroundColor DarkCyan
Write-Host ("  " + (_d '{}')) -ForegroundColor Cyan
Write-Host ("  " + (_d '{}') + (_d '{}')) -ForegroundColor Yellow
Write-Host ("  " + (_d '{}') + (_d '{}')) -ForegroundColor Gray
Write-Host "==================================================================" -ForegroundColor DarkCyan
Write-Host ""
Set-Location -LiteralPath (_d '{}')
$prog = _d '{}'
& $prog
"#,
        b64_utf8(&title),
        b64_utf8(&format!("轻舟 Multi-CLI Hub · 正在启动 {}", status.display_name)),
        b64_utf8("命令路径: "),
        b64_utf8(exe_path),
        b64_utf8("项目目录: "),
        b64_utf8(&workdir.to_string_lossy()),
        b64_utf8(&workdir.to_string_lossy()),
        b64_utf8(exe_path),
    );
    atomic_write_text(&ps1_path, &ps1_content)?;

    let ps_exe =
        find_in_path(&["pwsh.exe", "pwsh"]).unwrap_or_else(|| PathBuf::from("powershell.exe"));
    let cmd_content = build_cmd_launcher_content(&ps_exe, &ps1_path);
    atomic_write_text(&cmd_path, &cmd_content)?;

    let host = detect_terminal_host();
    match &host {
        TerminalHost::WindowsTerminal(wt_path) => {
            StdCommand::new(wt_path)
                .args([
                    "new-tab",
                    "--title",
                    &title,
                    "-d",
                    &workdir.to_string_lossy(),
                    "cmd.exe",
                    "/D",
                    "/K",
                    &cmd_path.to_string_lossy(),
                ])
                .spawn()
                .map_err(|e| format!("拉起 Windows Terminal 失败: {e}"))?;
        }
        TerminalHost::Pwsh(_) | TerminalHost::PowerShell(_) | TerminalHost::Cmd(_) => {
            let mut cmd = StdCommand::new("cmd.exe");
            cmd.current_dir(&workdir)
                .args(["/D", "/K", &cmd_path.to_string_lossy()]);
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

    Ok(format!(
        "已在 {} 中拉起 {} 独立交互终端",
        host.display_name(),
        status.display_name
    ))
}
