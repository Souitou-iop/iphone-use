//! The few OS calls that differ between macOS and Windows.
//!
//! On Unix these are the std/libc calls the daemon always made. On Windows
//! the file-permission bits are no-ops: the files live under the user's
//! profile (`%LOCALAPPDATA%`, `%TEMP%`), whose ACL already keeps other
//! accounts out, and Windows has no uid or mode bits to check.

#[cfg(unix)]
pub use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

/// `O_NOFOLLOW` where it exists; 0 (no extra flag) on Windows, where callers
/// check `symlink_metadata` first.
#[cfg(unix)]
pub const O_NOFOLLOW: i32 = libc::O_NOFOLLOW;
#[cfg(windows)]
pub const O_NOFOLLOW: i32 = 0;

#[cfg(windows)]
pub trait OpenOptionsExt {
    fn mode(&mut self, mode: u32) -> &mut Self;
    fn custom_flags(&mut self, flags: i32) -> &mut Self;
}

#[cfg(windows)]
impl OpenOptionsExt for std::fs::OpenOptions {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
    fn custom_flags(&mut self, _flags: i32) -> &mut Self {
        self
    }
}

#[cfg(windows)]
pub trait DirBuilderExt {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

#[cfg(windows)]
impl DirBuilderExt for std::fs::DirBuilder {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

/// Broken-down local time for a unix timestamp.
pub fn local_tm(unix: u64) -> libc::tm {
    let t = unix as libc::time_t;
    // SAFETY: the call writes only into `tm`, which we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    #[cfg(unix)]
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    #[cfg(windows)]
    unsafe {
        libc::localtime_s(&mut tm, &t);
    }
    tm
}

/// Effective uid (Windows has none; its callers are Unix-only).
#[cfg(unix)]
pub fn euid() -> u32 {
    // SAFETY: no arguments, no preconditions, always succeeds.
    unsafe { libc::geteuid() }
}

/// `HOME`, or on Windows `%USERPROFILE%` when `HOME` is unset.
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("USERPROFILE")
            } else {
                None
            }
        })
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
}

/// Windows shells do not set `HOME`; the daemon keeps its state under it
/// (`~/.iphone-use`, flows, caches), so point it at the user profile. Call
/// first thing in `main`, before any thread starts.
pub fn ensure_home() {
    if std::env::var_os("HOME").map_or(true, |home| home.is_empty()) {
        if let Some(home) = home_dir() {
            std::env::set_var("HOME", home);
        }
    }
}
