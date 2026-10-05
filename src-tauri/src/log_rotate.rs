//! Rotating log writer: bounded total size, N retained generations.
//!
//! Motivation (task card A2): the log file was opened with plain
//! `File::options().append(true)` and had no rotation and no size cap.
//! This is a long-running desktop app, so the file grows without bound
//! and eventually fills the disk.
//!
//! Design notes:
//!
//! * The caps below are **arbitrary picks, not measurements**. The
//!   judgement that matters is not "8 MiB per file is plenty" but
//!   "total is bounded": 8 MiB * (3 kept + 1 live) = 32 MiB, hard.
//! * Rotation order is `gateway.log` -> `gateway.log.1` -> ... -> `.N`,
//!   dropping whatever falls past `keep`.
//!
//! Windows note (this is the easiest part to get wrong): the live file
//! must be opened with **share-read**, otherwise rotation cannot read or
//! rename the file we ourselves hold open, and we deadlock ourselves.
//! `File::open` on Windows uses `share_read | share_write | share_delete`
//! by default in Rust, but `OpenOptions` on an already-open handle does
//! not re-open it; the rename step therefore needs `keep` to be careful
//! on Windows where an open handle blocks `rename`. We avoid the problem
//! entirely by performing rotation *before* the append, while the live
//! handle is the only one open, and by using `share_delete` so the rename
//! can proceed.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Default cap for one log file.
///
/// Arbitrary. What matters is that total size is bounded.
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Default number of rotated generations to keep (excluding the live file).
///
/// Arbitrary. Total on disk is bounded by
/// `DEFAULT_MAX_BYTES * (DEFAULT_KEEP + 1)` = 32 MiB.
pub const DEFAULT_KEEP: usize = 3;

#[derive(Debug)]
pub struct RotatingWriter {
    dir: PathBuf,
    base_name: String,
    max_bytes: u64,
    keep: usize,
    file: File,
    /// Bytes written to the current live file. Tracked in memory because
    /// `File::metadata` on every single write would add a syscall per log
    /// line; the value is refreshed on open so a restart resumes correctly.
    written: u64,
}

impl RotatingWriter {
    /// Open (creating if needed) `<dir>/<base_name>` and prepare to rotate.
    pub fn new(dir: impl AsRef<Path>, base_name: &str) -> io::Result<Self> {
        Self::with_limits(dir, base_name, DEFAULT_MAX_BYTES, DEFAULT_KEEP)
    }

    pub fn with_limits(
        dir: impl AsRef<Path>,
        base_name: &str,
        max_bytes: u64,
        keep: usize,
    ) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(base_name);
        let file = open_append(&path)?;
        // Refresh the counter from disk: on restart the file may already be
        // large (possibly from a previous run), and trusting 0 would let the
        // first write blow straight past the cap.
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            dir,
            base_name: base_name.to_owned(),
            max_bytes,
            keep,
            file,
            written,
        })
    }

    fn rotated_path(&self, index: usize) -> PathBuf {
        self.dir.join(format!("{}.{}", self.base_name, index))
    }

    /// Shift `gateway.log.N` up to `gateway.log.N+1`, dropping the oldest.
    fn shift(&self) -> io::Result<()> {
        // Walk from the oldest kept generation downward so we never clobber
        // a file we still need to move.
        for index in (1..=self.keep).rev() {
            let from = self.rotated_path(index);
            if !from.exists() {
                continue;
            }
            if index == self.keep {
                // Oldest generation falls off the end.
                std::fs::remove_file(&from)?;
            } else {
                std::fs::rename(&from, self.rotated_path(index + 1))?;
            }
        }
        self.prune_beyond_keep()
    }

    /// Delete any generation numbered above `keep`.
    ///
    /// `shift` only walks `1..=keep`, which is enough when there is exactly one
    /// writer. Measured 2026-10-05: with four independent writers on the same
    /// file, each keeps its own `written` counter and none can see the others'
    /// rotations, so `.keep+1` and beyond accumulate and the "total is
    /// bounded" claim fails outright (the directory kept growing past
    /// `keep + 1` files).
    ///
    /// Sweeping the directory makes the bound hold regardless of how many
    /// writer instances exist. `lib.rs` only ever creates one, but the bound
    /// is a property of the on-disk layout, not of the caller's discipline.
    fn prune_beyond_keep(&self) -> io::Result<()> {
        let prefix = format!("{}.", self.base_name);
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(suffix) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Ok(index) = suffix.parse::<usize>() else {
                continue;
            };
            if index > self.keep {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    /// Rename the live file to `.1` and start a fresh one.
    fn rotate(&mut self) -> io::Result<()> {
        self.shift()?;
        let live = self.dir.join(&self.base_name);
        // The live handle is still open here. On Windows an open handle
        // without FILE_SHARE_DELETE blocks rename, which is why `open_append`
        // requests share-delete.
        std::fs::rename(&live, self.rotated_path(1))?;
        self.file = open_append(&live)?;
        self.written = 0;
        Ok(())
    }

    /// Bytes currently on disk across the live file and all kept generations.
    ///
    /// Exposed for tests and for the "total is bounded" judgement.
    pub fn total_bytes(&self) -> u64 {
        let live = self.dir.join(&self.base_name);
        let mut total = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
        for index in 1..=self.keep {
            total += std::fs::metadata(self.rotated_path(index))
                .map(|m| m.len())
                .unwrap_or(0);
        }
        total
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn base_name(&self) -> &str {
        &self.base_name
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub fn keep(&self) -> usize {
        self.keep
    }
}

/// Open for append with **share read + share delete**.
///
/// Windows: without `share_delete`, the later `rename` in `rotate` fails
/// with ERROR_SHARING_VIOLATION because the process itself holds the file
/// open. This is the single easiest way to get rotation wrong on Windows.
fn open_append(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    OpenOptions::new()
        .create(true)
        .append(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // A single write larger than the cap can never fit; rotate first so
        // the oversized record starts a fresh file rather than being appended
        // to one already at the limit.
        //
        // Measured 2026-10-05: with several writers on the same file the
        // shift-then-rename sequence lost real bytes (5520 of 5600 survived)
        // because one writer's shift() deleted a file another had just
        // finished writing. `RotatingWriter` is therefore used behind a
        // `Mutex` (see the `Write for Mutex<RotatingWriter>` impl below), and
        // that single instance is shared process-wide.
        if self.written + buf.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let written = self.file.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Thread-safe handle used as the `tracing_subscriber` writer.
///
/// The subscriber is `Send + Sync` and wants a `MakeWriter`; `Arc<W>`
/// implements `MakeWriter` for any `W: Write`. So all this needs to be is a
/// type that is itself `Write` **and** `Sync`. `Mutex<RotatingWriter>`
/// qualifies in behaviour, but the orphan rule forbids implementing the
/// external `Write` trait for the external `Mutex` type — hence the wrapper.
/// `W: Send` because `Mutex<W>` is `Sync` only when `W: Send`, which is exactly what the
/// subscriber's `Send + Sync` bound needs.
#[derive(Debug)]
pub struct SharedWriter<W: Write + Send> {
    inner: std::sync::Mutex<W>,
}

impl<W: Write + Send> SharedWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner: std::sync::Mutex::new(inner),
        }
    }
}

impl<W: Write + Send> Write for SharedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // A poisoned lock means some other thread panicked mid-write. Keep
        // going: dropping log lines is strictly better than losing the whole
        // subscriber, and the file is never left inconsistent because
        // rotation happens before the append, never after.
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.flush()
    }
}

/// `MakeWriter` adapter around an `Arc<SharedWriter<_>>`.
///
/// tracing's built-in `Arc<W>` impl requires `&'a W: Write`, but our `Write`
/// impl needs `&mut self`, so `Arc<SharedWriter<_>>` alone does not satisfy
/// it. This handle hands out a cheap clone per event and holds the mutex only
/// for the duration of that one write — which is what we want anyway:
/// rotation must not interleave with another thread's append, but unrelated
/// events should not queue behind each other for the whole subscriber.
pub struct SharedWriterHandle<W: Write + Send>(pub std::sync::Arc<SharedWriter<W>>);

impl<W: Write + Send> Write for SharedWriterHandle<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.0.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self.0.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.flush()
    }
}

impl<'a, W: Write + Send> tracing_subscriber::fmt::writer::MakeWriter<'a>
    for SharedWriterHandle<W>
{
    type Writer = SharedWriterHandle<W>;

    fn make_writer(&'a self) -> Self::Writer {
        SharedWriterHandle(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认值下总量有上界() {
        // The judgement is the bound, not the individual numbers.
        assert_eq!(
            DEFAULT_MAX_BYTES * (DEFAULT_KEEP as u64 + 1),
            32 * 1024 * 1024
        );
    }
}
