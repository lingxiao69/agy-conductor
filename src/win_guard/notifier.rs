/// 完工时唤醒主窗口（提示音 + 任务栏闪烁 + 置顶唤起轻舟窗口展示应用内紧凑滚动报告弹窗）。
/// 仅当主窗口句柄为空时才兜底弹出 <=200 字的极简系统摘要框，彻底告别撑满全屏的巨型 MessageBoxW。
pub fn notify_project_completed(hwnd_raw: Option<isize>, summary_text: String) {
    tokio::task::spawn_blocking(move || {
        wake_window_or_short_fallback(
            hwnd_raw,
            "项目开发验收完成 — AgyConductor",
            &summary_text,
            false,
        );
    });
}

/// 阻塞升级需用户拍板时唤醒主窗口，聚焦至应用内紧凑滚动拍板弹窗与底部输入框。
pub fn notify_blocker_needs_user(
    hwnd_raw: Option<isize>,
    title: String,
    plain_explanation: String,
) {
    tokio::task::spawn_blocking(move || {
        let final_title = if title.trim().is_empty() {
            "需要您拍板 — AgyConductor".to_string()
        } else if title.contains("AgyConductor") {
            title
        } else {
            format!("{title} — AgyConductor")
        };
        wake_window_or_short_fallback(hwnd_raw, &final_title, &plain_explanation, true);
    });
}

#[cfg(windows)]
fn to_wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn build_short_fallback_body(raw: &str, is_warning: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        let cleaned = line.trim().trim_start_matches('#').trim();
        if cleaned.is_empty() {
            continue;
        }
        lines.push(cleaned.to_string());
        if lines.len() >= 3 {
            break;
        }
    }
    let mut text = lines.join("\n");
    let char_count = text.chars().count();
    if char_count > 160 {
        text = text.chars().take(160).collect::<String>();
        text.push_str("...");
    }
    let footer = if is_warning {
        "请打开轻舟主窗口查看详情并回复拍板。"
    } else {
        "请打开轻舟主窗口查看完整验收报告。"
    };
    if text.is_empty() {
        footer.to_string()
    } else {
        format!("{text}\n\n{footer}")
    }
}

#[cfg(windows)]
fn wake_window_or_short_fallback(
    hwnd_raw: Option<isize>,
    title: &str,
    body: &str,
    is_warning: bool,
) {
    use std::ffi::c_void;
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FlashWindowEx, IsIconic, MessageBoxW, SetForegroundWindow, SetWindowPos, ShowWindow,
        FLASHWINFO, FLASHW_ALL, FLASHW_TIMERNOFG, HWND_NOTOPMOST, HWND_TOPMOST, MB_ICONASTERISK,
        MB_ICONEXCLAMATION, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND,
        MB_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SW_RESTORE,
    };

    let hwnd: HWND = hwnd_raw
        .filter(|&raw| raw != 0)
        .map(|raw| raw as *mut c_void)
        .unwrap_or(std::ptr::null_mut());

    let beep_kind = if is_warning {
        MB_ICONEXCLAMATION
    } else {
        MB_ICONASTERISK
    };
    unsafe {
        let _ = MessageBeep(beep_kind);
    }

    if !hwnd.is_null() {
        let flash = FLASHWINFO {
            cbSize: size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 6,
            dwTimeout: 0,
        };
        unsafe {
            let _ = FlashWindowEx(&flash);
            if IsIconic(hwnd) != 0 {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let flags = SWP_NOMOVE | SWP_NOSIZE;
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags);
            let _ = SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, flags);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }

    // 仅当主窗口句柄不可用（例如无头模式）时兜底弹简短 4 行摘要框。
    let icon_flag = if is_warning {
        MB_ICONWARNING
    } else {
        MB_ICONINFORMATION
    };
    let short_body = build_short_fallback_body(body, is_warning);
    let title_w = to_wide_null(title);
    let body_w = to_wide_null(&short_body);
    unsafe {
        let _ = MessageBoxW(
            std::ptr::null_mut(),
            body_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | icon_flag | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(windows))]
fn wake_window_or_short_fallback(
    _hwnd_raw: Option<isize>,
    _title: &str,
    _body: &str,
    _is_warning: bool,
) {
}
