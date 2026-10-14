//! Private Windows job for a suspended CLI child. Descendants remain owned after parent exit.
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::*,
        JobObjects::*,
        Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};
pub(crate) struct CliJob(HANDLE);
// Windows Job handles are not thread-affine. Kernel operations through shared references
// do not mutate Rust memory; the single owner closes the handle only after borrows end.
unsafe impl Send for CliJob {}
unsafe impl Sync for CliJob {}
impl CliJob {
    pub(crate) fn new() -> Result<Self, String> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err("无法建立 CLI 自有进程组".into());
        }
        let job = Self(handle);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as _,
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err("无法配置 CLI 进程组清理".into());
        }
        Ok(job)
    }
    pub(crate) fn assign_and_resume(&self, child: &tokio::process::Child) -> Result<(), String> {
        let process = child.raw_handle().ok_or("CLI 进程句柄不可用")?;
        if unsafe { AssignProcessToJobObject(self.0, process as _) } == 0 {
            return Err("无法绑定 CLI 自有进程组；已停止启动".into());
        }
        let pid = child.id().ok_or("CLI 进程尚未就绪")?;
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err("无法读取自有 CLI 启动线程".into());
        }
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut next = unsafe { Thread32First(snapshot, &mut entry) };
        let mut resumed = false;
        while next != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !thread.is_null() {
                    let result = unsafe { ResumeThread(thread) };
                    unsafe { CloseHandle(thread) };
                    if result != u32::MAX && result > 0 {
                        resumed = true;
                    }
                }
            }
            next = unsafe { Thread32Next(snapshot, &mut entry) };
        }
        unsafe { CloseHandle(snapshot) };
        if !resumed {
            return Err("无法恢复自有 CLI 启动线程".into());
        }
        Ok(())
    }
    pub(crate) fn stop(&self) -> Result<(), String> {
        if unsafe { TerminateJobObject(self.0, 1) } == 0 {
            return Err("无法停止 CLI 自有进程组".into());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as _,
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err("无法确认 CLI 进程组退出".into());
            }
            if info.ActiveProcesses == 0 {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err("CLI 进程组退出未能及时确认".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
impl Drop for CliJob {
    fn drop(&mut self) {
        let _ = self.stop();
        unsafe { CloseHandle(self.0) };
    }
}
