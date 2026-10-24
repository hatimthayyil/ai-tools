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
