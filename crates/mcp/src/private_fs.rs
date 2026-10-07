//! Owner-only files and directories, on every platform.
//!
//! On Unix this is the mode the flow store, drafts and run evidence have always
//! been created with (0600 files, 0700 directories) and `O_NOFOLLOW`. Windows
//! has no mode bits: files under the user profile inherit its owner-only ACL,
//! so what remains to enforce is never following a link someone planted.

use std::fs::{DirBuilder, OpenOptions};

/// `OpenOptions` that create a file readable and writable by its owner only.
pub fn file_options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

/// A `DirBuilder` that creates directories accessible by their owner only.
pub fn dir_builder() -> DirBuilder {
    #[allow(unused_mut)]
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
}

/// Open the last path component itself rather than what a symlink there
/// points to. On Unix that is `O_NOFOLLOW` (the open fails). On Windows it is
/// `FILE_FLAG_OPEN_REPARSE_POINT`: the link itself opens, and its metadata
/// then reads as a symlink, which every caller's `is_file()` check refuses.
pub fn no_follow(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}
