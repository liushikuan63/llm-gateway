//! One OS-backed worker lease per scratch profile, released on process exit.
//! A file's existence is never interpreted as ownership; the live handle is.
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

pub(super) struct ProcessLease {
    _file: File,
}
pub(super) fn try_acquire(root: &Path) -> Result<Option<ProcessLease>, String> {
    let path = root.join("owner.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(windows))]
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(super::ERROR.into());
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        #[cfg(windows)]
        Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => return Ok(None),
        Err(_) => return Err(super::ERROR.into()),
    };
    if super::linked(&file.metadata().map_err(|_| super::ERROR.to_owned())?) {
        return Err(super::ERROR.into());
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        extern "C" {
            fn flock(
                fd: std::os::raw::c_int,
                operation: std::os::raw::c_int,
            ) -> std::os::raw::c_int;
        }
        // flock is scoped to the open file description, so File remains Send and
        // no thread-specific mutex ownership is involved in asynchronous workers.
        if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(super::ERROR.into());
        }
    }
    Ok(Some(ProcessLease { _file: file }))
}
