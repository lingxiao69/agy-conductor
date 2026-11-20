use crate::agy::terminal::TerminalLaunchOptions;
use crate::orchestrator::types::AgentSlot;
use std::path::Path;

pub(crate) fn build_ps_banner_header(
    title: &str,
    slot: Option<&AgentSlot>,
    workdir: &Path,
    mode_desc: &str,
) -> String {
    let role_line = match slot {
        Some(s) => format!(
            "{} (@{}, {}, P{})",
            s.display_name,
            s.alias,
            s.role.label_zh(),
            s.normalized_priority()
        ),
        None => "全部活跃角色 / 独立交互会话".to_string(),
    };
    let model_line = slot
        .map(|s| s.model_id.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("默认探测模型");
    let conv_line = slot
        .and_then(|s| s.conversation_id.as_deref())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("尚未建立 / 新建会话");

    // 全部中文文案使用纯 ASCII Base64 + UTF-8 运行时解码，确保 UTF-8 无 BOM 脚本在 Windows PowerShell 5.1 零乱码
    format!(
        r#"[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
function _d([string]$b) {{ [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String($b)) }}
$Host.UI.RawUI.WindowTitle = _d '{}'
Write-Host ""
Write-Host "==================================================================" -ForegroundColor DarkCyan
Write-Host ("  " + (_d '{}')) -ForegroundColor Cyan
Write-Host ("  " + (_d '{}') + (_d '{}')) -ForegroundColor Green
Write-Host ("  " + (_d '{}') + (_d '{}')) -ForegroundColor White
Write-Host ("  " + (_d '{}') + (_d '{}')) -ForegroundColor Yellow
Write-Host ("  " + (_d '{}') + (_d '{}')) -ForegroundColor Gray
Write-Host ("  " + (_d '{}') + (_d '{}')) -ForegroundColor DarkGray
Write-Host "==================================================================" -ForegroundColor DarkCyan
Write-Host ""
"#,
        b64_utf8(title),
        b64_utf8("轻舟 AgyConductor · 终端窗口已就绪"),
        b64_utf8("当前模式: "),
        b64_utf8(mode_desc),
        b64_utf8("角色身份: "),
        b64_utf8(&role_line),
        b64_utf8("绑定模型: "),
        b64_utf8(model_line),
        b64_utf8("项目目录: "),
        b64_utf8(&workdir.to_string_lossy()),
        b64_utf8("会话标识: "),
        b64_utf8(conv_line),
    )
}

pub(crate) fn build_live_tail_ps1(
    title: &str,
    slot: Option<&AgentSlot>,
    workdir: &Path,
    log_file: &Path,
) -> String {
    let mut s = build_ps_banner_header(
        title,
        slot,
        workdir,
        "实时 AI 进程追踪流（只读围观，不阻塞后台协同，按 Ctrl+C 退出追踪）",
    );
    s.push_str(&format!(
        r#"$logPath = _d '{}'
Write-Host ((_d '{}') + $logPath) -ForegroundColor DarkGray
Write-Host (_d '{}') -ForegroundColor DarkYellow
Write-Host ""
if (-not (Test-Path -LiteralPath $logPath)) {{
  New-Item -ItemType File -Path $logPath -Force | Out-Null
}}
Get-Content -LiteralPath $logPath -Encoding UTF8 -Wait -Tail 80 | ForEach-Object {{
  $line = $_
  if ($line -match '\[错误\]') {{ Write-Host $line -ForegroundColor Red }}
  elseif ($line -match '\[工具\]') {{ Write-Host $line -ForegroundColor Green }}
  elseif ($line -match '\[思考\]') {{ Write-Host $line -ForegroundColor Magenta }}
  elseif ($line -match '\[结果\]') {{ Write-Host $line -ForegroundColor Yellow }}
  elseif ($line -match '\[系统\]') {{ Write-Host $line -ForegroundColor Cyan }}
  else {{ Write-Host $line -ForegroundColor Gray }}
}}
"#,
        b64_utf8(&log_file.to_string_lossy()),
        b64_utf8("日志文件: "),
        b64_utf8("正在实时等待 AI 思考与工具调用日志..."),
    ));
    s
}

pub(crate) fn build_interactive_cli_ps1(
    title: &str,
    slot: Option<&AgentSlot>,
    workdir: &Path,
    agy_cmd: &[String],
    opts: &TerminalLaunchOptions,
) -> String {
    let mode_desc = if opts.attach_conversation {
        "交互式 agy CLI（接管当前对话上下文）"
    } else {
        "交互式 agy CLI（正在连接本地网关与模型服务，请稍候...）"
    };
    let mut s = build_ps_banner_header(title, slot, workdir, mode_desc);
    let program = agy_cmd.first().map(String::as_str).unwrap_or("agy");
    let args_array = agy_cmd
        .iter()
        .skip(1)
        .map(|a| format!("(_d '{}')", b64_utf8(a)))
        .collect::<Vec<_>>()
        .join(", ");
    s.push_str(&format!(
        r#"Set-Location -LiteralPath (_d '{}')
$prog = _d '{}'
$cliArgs = @({})
Write-Host (_d '{}') -ForegroundColor DarkCyan
& $prog @cliArgs
"#,
        b64_utf8(&workdir.to_string_lossy()),
        b64_utf8(program),
        args_array,
        b64_utf8("正在启动 agy 交互式终端，若为首次拉起 Gateway 同步模型可能需要 2~5 秒..."),
    ));
    s
}

pub(crate) fn build_cmd_launcher_content(ps_exe: &Path, ps1_path: &Path) -> String {
    format!(
        "@echo off\r\nchcp 65001 >nul\r\n\"{}\" -NoLogo -NoExit -ExecutionPolicy Bypass -File \"{}\"\r\n",
        ps_exe.to_string_lossy(),
        ps1_path.to_string_lossy()
    )
}

pub(crate) fn b64_utf8(input: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
