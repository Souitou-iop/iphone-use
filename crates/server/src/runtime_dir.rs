//! Per-user runtime directory for pid/log/secret files.
//!
//! The directory lives at `$TMPDIR/hermes-phone-remote-$UID` (falling back to
//! `/tmp/hermes-phone-remote-$UID` when `$TMPDIR` is unset).  It is created
//! with mode `0700`; every access validates ownership and permissions so that
//! an adversary who controls other paths under `/tmp` cannot trick the process
//! into reading or writing their files.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Return (and, if necessary, create) the per-user runtime directory.
///
/// The path is `$TMPDIR/hermes-phone-remote-$UID` (`-<instance>` appended for
/// a named instance), falling back to
/// `/tmp/hermes-phone-remote-$UID` when `$TMPDIR` is unset or empty.
///
/// If the directory already exists its owner and mode are validated; an
/// `io::Error` with kind `PermissionDenied` is returned if either check fails.
pub fn runtime_dir() -> io::Result<PathBuf> {
    let uid = current_uid();
    // Windows has no $TMPDIR convention; std's temp dir is per-user there.
    #[cfg(unix)]
    let base = std::env::var("TMPDIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/tmp".to_owned());
    #[cfg(not(unix))]
    let base = std::env::temp_dir().to_string_lossy().into_owned();
    let dir = PathBuf::from(base).join(dir_name(uid, crate::instance::current()));
    ensure_dir(&dir)?;
    Ok(dir)
}

/// The pid record and session secret are per daemon: a named instance (#67)
/// gets its own directory so the default daemon's live pid record does not
/// read as "already running" to it.
fn dir_name(uid: u32, instance: &crate::instance::Instance) -> String {
    if instance.is_default() {
        format!("hermes-phone-remote-{uid}")
    } else {
        format!("hermes-phone-remote-{uid}-{}", instance.name)
    }
}

/// Create `dir/name` atomically with mode `0600`, writing `bytes`.
///
/// Fails with `AlreadyExists` if the file is already present (uses
/// `O_CREAT|O_EXCL`).  Never follows symlinks (`O_NOFOLLOW`).
pub fn write_secret(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    write_secret_in(dir, name, bytes)
}

/// Read `dir/name`, first validating that the file is a regular file owned by
/// the current uid with mode `0600`.
///
/// Returns `io::Error(PermissionDenied)` if any security check fails.  Opens
/// with `O_NOFOLLOW` so symlinks are rejected at the OS level.
pub fn read_secret(dir: &Path, name: &str) -> io::Result<Vec<u8>> {
    read_secret_in(dir, name)
}

// ---------------------------------------------------------------------------
// Internal helpers (also used directly by tests to avoid real-$TMPDIR
// dependency)
// ---------------------------------------------------------------------------

/// Ensure `dir` exists with mode 0700 and is owned by the current uid.
///
/// Creates the directory if it does not exist; validates owner + mode if it
/// does.
pub(crate) fn ensure_dir(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        validate_dir(dir)
    } else {
        // Create with the correct mode in one step.
        // `std::fs::create_dir` uses umask, so we set mode explicitly via
        // `std::os::unix::fs::DirBuilder`. Windows has no mode bits; the
        // per-user temp dir scopes the directory's ACL.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(dir)?;
        Ok(())
    }
}

/// Validate that `dir` is owned by the current uid and has mode exactly 0700.
pub(crate) fn validate_dir(dir: &Path) -> io::Result<()> {
    let meta = fs::metadata(dir)?; // follows symlinks — we want the dir itself
    // Owner and mode checks are Unix-only: Windows has no POSIX uid or mode
    // bits, and the per-user temp dir already scopes the directory's ACL.
    #[cfg(unix)]
    {
        let uid = current_uid();
        if meta.uid() != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "runtime dir {:?} is owned by uid {} but current uid is {}",
                    dir,
                    meta.uid(),
                    uid
                ),
            ));
        }
        // Mode bits: mask off the file-type bits, keep only the permission bits.
        let mode = meta.mode() & 0o7777;
        if mode != 0o700 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "runtime dir {:?} has mode {:04o} but expected 0700",
                    dir, mode
                ),
            ));
        }
    }
    #[cfg(not(unix))]
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("runtime dir {:?} is not a directory", dir),
        ));
    }
    Ok(())
}

/// Internal: atomically create `dir/name` with `O_CREAT|O_EXCL|O_NOFOLLOW`,
/// mode `0600`.
pub(crate) fn write_secret_in(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let path = dir.join(name);
    // O_NOFOLLOW: reject the open if the final path component is a symlink.
    // O_EXCL:     fail if the file already exists (create_new implies it too).
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        OpenOptions::new()
            .write(true)
            .create_new(true) // implies O_CREAT | O_EXCL
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_EXCL) // i32 from libc constants
            .open(&path)?
    };
    #[cfg(not(unix))]
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
    file.write_all(bytes)?;
    Ok(())
}

/// Internal: validate and read `dir/name` with `O_NOFOLLOW`.
pub(crate) fn read_secret_in(dir: &Path, name: &str) -> io::Result<Vec<u8>> {
    let path = dir.join(name);

    // lstat so we inspect the link itself, not its target.
    let meta = path.symlink_metadata()?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("secret file {:?} is not a regular file", path),
        ));
    }
    // Owner and mode checks are Unix-only (see `validate_dir`).
    #[cfg(unix)]
    {
        let uid = current_uid();
        if meta.uid() != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "secret file {:?} is owned by uid {} but current uid is {}",
                    path,
                    meta.uid(),
                    uid
                ),
            ));
        }
        let mode = meta.mode() & 0o7777;
        if mode != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "secret file {:?} has mode {:04o} but expected 0600",
                    path, mode
                ),
            ));
        }
    }

    // Open with O_NOFOLLOW so the OS also rejects a symlink (belt-and-suspenders).
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW) // i32 from libc
            .open(&path)?
    };
    #[cfg(not(unix))]
    let mut file = OpenOptions::new().read(true).open(&path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Return the effective UID of the current process (0 on platforms without a
/// POSIX uid).
#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, has no preconditions, and always
    // succeeds.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn a_named_instance_gets_its_own_runtime_dir() {
        let default = crate::instance::Instance::derive("", "/Users/x", None).unwrap();
        let lab = crate::instance::Instance::derive("lab", "/Users/x", None).unwrap();
        assert_eq!(dir_name(501, &default), "hermes-phone-remote-501");
        assert_eq!(dir_name(501, &lab), "hermes-phone-remote-501-lab");
    }

    // Helper: create a fresh 0700 tempdir.
    fn tmp700() -> TempDir {
        let td = TempDir::new().unwrap();
        fs::set_permissions(td.path(), fs::Permissions::from_mode(0o700)).unwrap();
        td
    }

    #[test]
    fn write_then_read_roundtrip() {
        let td = tmp700();
        write_secret_in(td.path(), "tok", b"hello secret").unwrap();
        let got = read_secret_in(td.path(), "tok").unwrap();
        assert_eq!(got, b"hello secret");
    }

    #[test]
    fn write_refuses_to_clobber_existing() {
        let td = tmp700();
        write_secret_in(td.path(), "tok", b"first").unwrap();
        let err = write_secret_in(td.path(), "tok", b"second").unwrap_err();
        // O_EXCL → AlreadyExists
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn read_refuses_wrong_mode() {
        let td = tmp700();
        write_secret_in(td.path(), "tok", b"data").unwrap();
        // Relax permissions to 0644 — read_secret_in must reject this.
        let path = td.path().join("tok");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_secret_in(td.path(), "tok").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn read_refuses_symlink() {
        let td = tmp700();
        // Create the real file in a separate temp location.
        let real_td = TempDir::new().unwrap();
        let real_path = real_td.path().join("real");
        std::fs::write(&real_path, b"data").unwrap();
        // Place a symlink inside our 0700 dir pointing to the real file.
        let link_path = td.path().join("sym");
        std::os::unix::fs::symlink(&real_path, &link_path).unwrap();
        // lstat on the link shows it is *not* a regular file → rejected.
        let err = read_secret_in(td.path(), "sym").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn validate_dir_rejects_non_700() {
        let td = TempDir::new().unwrap();
        // tempfile creates dirs with 0700 on most systems; set 0755 explicitly.
        fs::set_permissions(td.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let err = validate_dir(td.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn ensure_dir_creates_with_0700() {
        let parent = TempDir::new().unwrap();
        let target = parent.path().join("new-dir");
        ensure_dir(&target).unwrap();
        let meta = fs::metadata(&target).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o700);
    }

    #[test]
    fn ensure_dir_accepts_valid_existing_dir() {
        let td = tmp700();
        // Should succeed: dir owned by us with mode 0700.
        ensure_dir(td.path()).unwrap();
    }
}
