use std::path::Path;

#[derive(Debug, Clone)]
pub struct OpenCodeRawMessageRow {
    pub id: String,
    pub session_id: String,
    pub time_created: i64,
    pub data_json: String,
}

/// 严格白名单表名，仅允许只读访问 OpenCode 消息表，严禁接触 `account` / `credential` 等敏感表。
const ALLOWED_OPENCODE_TABLES: &[&str] = &["message", "session_message"];

#[cfg(windows)]
pub fn query_opencode_messages_readonly(db_path: &Path) -> Result<Vec<OpenCodeRawMessageRow>, String> {
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };

    const SQLITE_OK: c_int = 0;
    const SQLITE_ROW: c_int = 100;
    const SQLITE_OPEN_READONLY: c_int = 0x0000_0001;

    type FnOpenV2 = unsafe extern "C" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
    type FnCloseV2 = unsafe extern "C" fn(*mut c_void) -> c_int;
    type FnBusyTimeout = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
    type FnPrepareV2 = unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void, *mut *const c_char) -> c_int;
    type FnStep = unsafe extern "C" fn(*mut c_void) -> c_int;
    type FnColText = unsafe extern "C" fn(*mut c_void, c_int) -> *const u8;
    type FnColBytes = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
    type FnColInt64 = unsafe extern "C" fn(*mut c_void, c_int) -> i64;
    type FnFinalize = unsafe extern "C" fn(*mut c_void) -> c_int;

    struct DllGuard(HMODULE);
    impl Drop for DllGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = FreeLibrary(self.0);
                }
            }
        }
    }

    let wide_dll: Vec<u16> = "winsqlite3.dll"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let module = unsafe {
        LoadLibraryExW(
            wide_dll.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err("未找到系统自带 winsqlite3.dll".to_string());
    }
    let _dll = DllGuard(module);

    macro_rules! load_sym {
        ($name:literal, $ty:ty) => {{
            let sym = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
            match sym {
                Some(ptr) => unsafe { std::mem::transmute::<_, $ty>(ptr) },
                None => return Err(format!("winsqlite3.dll 缺少导出符号 {}", $name)),
            }
        }};
    }

    let sqlite3_open_v2 = load_sym!("sqlite3_open_v2", FnOpenV2);
    let sqlite3_close_v2 = load_sym!("sqlite3_close_v2", FnCloseV2);
    let sqlite3_busy_timeout = load_sym!("sqlite3_busy_timeout", FnBusyTimeout);
    let sqlite3_prepare_v2 = load_sym!("sqlite3_prepare_v2", FnPrepareV2);
    let sqlite3_step = load_sym!("sqlite3_step", FnStep);
    let sqlite3_column_text = load_sym!("sqlite3_column_text", FnColText);
    let sqlite3_column_bytes = load_sym!("sqlite3_column_bytes", FnColBytes);
    let sqlite3_column_int64 = load_sym!("sqlite3_column_int64", FnColInt64);
    let sqlite3_finalize = load_sym!("sqlite3_finalize", FnFinalize);

    let path_str = db_path.to_string_lossy();
    let c_path = CString::new(path_str.as_bytes())
        .map_err(|_| "OpenCode 数据库路径包含非法空字节".to_string())?;

    let mut db: *mut c_void = std::ptr::null_mut();
    let open_rc = unsafe {
        sqlite3_open_v2(
            c_path.as_ptr(),
            &mut db,
            SQLITE_OPEN_READONLY,
            std::ptr::null(),
        )
    };
    if open_rc != SQLITE_OK || db.is_null() {
        if !db.is_null() {
            unsafe {
                let _ = sqlite3_close_v2(db);
            }
        }
        return Err(format!("只读打开 opencode.db 失败 (rc={open_rc})"));
    }

    struct DbGuard {
        db: *mut c_void,
        close_fn: FnCloseV2,
    }
    impl Drop for DbGuard {
        fn drop(&mut self) {
            if !self.db.is_null() {
                unsafe {
                    let _ = (self.close_fn)(self.db);
                }
            }
        }
    }
    let _db_guard = DbGuard {
        db,
        close_fn: sqlite3_close_v2,
    };
    unsafe {
        let _ = sqlite3_busy_timeout(db, 1200);
    }

    let read_text = |stmt: *mut c_void, col: c_int| -> String {
        unsafe {
            let ptr = sqlite3_column_text(stmt, col);
            if ptr.is_null() {
                return String::new();
            }
            let len = sqlite3_column_bytes(stmt, col);
            if len <= 0 {
                return CStr::from_ptr(ptr as *const c_char)
                    .to_string_lossy()
                    .into_owned();
            }
            let slice = std::slice::from_raw_parts(ptr, len as usize);
            String::from_utf8_lossy(slice).into_owned()
        }
    };

    // 1. 仅查询白名单表是否存在
    let mut present_tables: Vec<&str> = Vec::new();
    let meta_sql = CString::new(
        "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('message','session_message')",
    )
    .expect("valid SQL");
    let mut stmt: *mut c_void = std::ptr::null_mut();
    let mut tail: *const c_char = std::ptr::null();
    if unsafe { sqlite3_prepare_v2(db, meta_sql.as_ptr(), -1, &mut stmt, &mut tail) } == SQLITE_OK
        && !stmt.is_null()
    {
        while unsafe { sqlite3_step(stmt) } == SQLITE_ROW {
            let name = read_text(stmt, 0);
            for &allowed in ALLOWED_OPENCODE_TABLES {
                if name == allowed {
                    present_tables.push(allowed);
                }
            }
        }
        unsafe {
            let _ = sqlite3_finalize(stmt);
        }
    }

    // 2. 逐表只读提取消息行
    let mut rows = Vec::new();
    for table in present_tables {
        let sql_str = format!("SELECT id, session_id, time_created, data FROM {table}");
        let Ok(c_sql) = CString::new(sql_str) else {
            continue;
        };
        let mut qstmt: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { sqlite3_prepare_v2(db, c_sql.as_ptr(), -1, &mut qstmt, &mut tail) };
        if rc != SQLITE_OK || qstmt.is_null() {
            continue;
        }
        while unsafe { sqlite3_step(qstmt) } == SQLITE_ROW {
            let id = read_text(qstmt, 0);
            let session_id = read_text(qstmt, 1);
            let time_created = unsafe { sqlite3_column_int64(qstmt, 2) };
            let data_json = read_text(qstmt, 3);
            if !data_json.is_empty() {
                rows.push(OpenCodeRawMessageRow {
                    id,
                    session_id,
                    time_created,
                    data_json,
                });
            }
        }
        unsafe {
            let _ = sqlite3_finalize(qstmt);
        }
    }

    Ok(rows)
}

#[cfg(not(windows))]
pub fn query_opencode_messages_readonly(_db_path: &Path) -> Result<Vec<OpenCodeRawMessageRow>, String> {
    Ok(Vec::new())
}
