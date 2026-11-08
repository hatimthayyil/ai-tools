use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};

/// How often [`lock_before`] tries a held lock again.
const POLL: Duration = Duration::from_millis(50);
/// The file naming a store's format.
const FORMAT: &str = "format";

/// A path as the user would type it: fold `$HOME` to `~`.
pub fn pretty(p: &Path) -> String {
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    match std::env::home_dir().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => p.display().to_string(),
    }
}

pub trait AtPath<T> {
    fn at(self, p: &Path) -> Result<T>;
}

impl<T> AtPath<T> for io::Result<T> {
    fn at(self, p: &Path) -> Result<T> {
        self.map_err(|e| {
            let msg = e.to_string();
            let reason = msg.split(" (os error").next().unwrap_or(&msg);
            anyhow!("{}: {reason}.", pretty(p))
        })
    }
}

/// An advisory lock on a file, released when dropped.
pub struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The lock on the file `p`; `None` while another process holds it.
pub fn try_lock(p: &Path) -> Result<Option<Lock>> {
    lock_before(p, Instant::now())
}

/// The lock on the file `p`, waiting for it until `deadline`; `None` if another process still
/// holds it then.
pub fn lock_before(p: &Path, deadline: Instant) -> Result<Option<Lock>> {
    let f = File::create(p).at(p)?;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(Some(Lock(f))),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Err(e).at(p),
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        thread::sleep(left.min(POLL));
    }
}

/// The lock on the file `p`, waiting for it.
pub fn lock(p: &Path) -> Result<Lock> {
    let f = File::create(p).at(p)?;
    f.lock().at(p)?;
    Ok(Lock(f))
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
compile_error!("rename_new renames without replacing only on Linux and macOS.");

/// Renames `from` to `to` unless `to` exists, in one step, so a path made at `to` meanwhile is
/// never replaced: an error of kind [`io::ErrorKind::AlreadyExists`] if it exists.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(io::Error::other);
    let (from, to) = (c(from)?, c(to)?);
    // SAFETY: both paths are NUL-terminated strings that outlive the call.
    #[cfg(target_os = "linux")]
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    // SAFETY: as above.
    #[cfg(target_vendor = "apple")]
    let r = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    match r {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// The format the `store` store in `dir` names; `None` without a format file.
pub fn format(dir: &Path, store: &str) -> Result<Option<u32>> {
    let p = dir.join(FORMAT);
    let v = match fs::read(&p) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).at(&p),
    };
    std::str::from_utf8(&v)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .map(Some)
        .with_context(|| format!("The {store} store's format file is corrupt."))
}

/// Refuses the `store` store in `dir` unless its format file names `format`; a store without
/// one is new.
pub fn check_format(dir: &Path, format: u32, store: &str) -> Result<()> {
    let Some(v) = self::format(dir, store)? else {
        return Ok(());
    };
    match v {
        v if v == format => Ok(()),
        v if v > format => {
            bail!("This {store} store is format {v}; this ai reads up to {format}. Upgrade ai.")
        }
        v => bail!("This {store} store is format {v}, which this ai does not read."),
    }
}

/// Makes the store directory `dir` and its format file, naming `format`, before its first write.
pub fn ensure_format(dir: &Path, format: u32) -> Result<()> {
    fs::create_dir_all(dir).at(dir)?;
    let p = dir.join(FORMAT);
    match OpenOptions::new().write(true).create_new(true).open(&p) {
        Ok(mut f) => f.write_all(format!("{format}\n").as_bytes()).at(&p),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).at(&p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_new_moves_a_directory_and_never_replaces() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b, c) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("c"),
        );
        fs::create_dir(&a).unwrap();
        fs::write(a.join("f"), "x").unwrap();
        rename_new(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(fs::read_to_string(b.join("f")).unwrap(), "x");

        fs::create_dir(&c).unwrap();
        let e = rename_new(&b, &c).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert!(b.join("f").is_file());
        assert_eq!(fs::read_dir(&c).unwrap().count(), 0);

        let e = rename_new(&a, &c).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }
}
