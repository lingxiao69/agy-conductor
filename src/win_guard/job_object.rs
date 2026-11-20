use std::sync::Arc;

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::null;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// Wraps a Windows Job Object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    /// Only headless background `agy` / `node` processes are attached to this job;
    /// user-launched interactive terminals are intentionally excluded.
    pub struct ProcessJobGuard {
        handle: HANDLE,
    }

    // SAFETY: A Windows kernel HANDLE for a Job Object can be used from any thread.
    unsafe impl Send for ProcessJobGuard {}
    unsafe impl Sync for ProcessJobGuard {}

    impl ProcessJobGuard {
        pub fn new() -> Result<Self, String> {
            let handle = unsafe { CreateJobObjectW(null(), null()) };
            if handle.is_null() {
                return Err(format!(
                    "CreateJobObjectW failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };

            if ok == 0 {
                let err = std::io::Error::last_os_error();
                unsafe {
                    CloseHandle(handle);
                }
                return Err(format!("SetInformationJobObject failed: {err}"));
            }

            Ok(Self { handle })
        }

        pub fn assign_pid(&self, pid: u32) -> Result<(), String> {
            let proc_handle =
                unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
            if proc_handle.is_null() {
                return Err(format!(
                    "OpenProcess(pid={pid}) failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            let ok = unsafe { AssignProcessToJobObject(self.handle, proc_handle) };
            let err = if ok == 0 {
                Some(std::io::Error::last_os_error())
            } else {
                None
            };
            unsafe {
                CloseHandle(proc_handle);
            }

            if let Some(e) = err {
                return Err(format!("AssignProcessToJobObject(pid={pid}) failed: {e}"));
            }
            Ok(())
        }

        pub fn terminate_all(&self) {
            unsafe {
                let _ = TerminateJobObject(self.handle, 1);
            }
        }
    }

    impl Drop for ProcessJobGuard {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe {
                    CloseHandle(self.handle);
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct ProcessJobGuard;

    impl ProcessJobGuard {
        pub fn new() -> Result<Self, String> {
            Ok(Self)
        }

        pub fn assign_pid(&self, _pid: u32) -> Result<(), String> {
            Ok(())
        }

        pub fn terminate_all(&self) {}
    }
}

pub use imp::ProcessJobGuard;

pub type SharedJobGuard = Arc<ProcessJobGuard>;

pub fn create_shared_job_guard() -> Option<SharedJobGuard> {
    ProcessJobGuard::new().ok().map(Arc::new)
}
