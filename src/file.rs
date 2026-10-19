use std::{
    fs::{File, TryLockError},
    io,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};

/// How often [`lock_before`] tries a held lock again.
const POLL: Duration = Duration::from_millis(50);

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
