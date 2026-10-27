use std::{
    fs::{File, OpenOptions},
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result};
use tracing::{level_filters::LevelFilter, subscriber::DefaultGuard};
use tracing_subscriber::EnvFilter;

use crate::file::AtPath;

pub const SUMMARISE_LOG: &str = "summarise.log";

/// The log at `path`, opened for appending.
pub fn open(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .at(path)
}

/// Sends this thread's events to the log at `path`, filtered by `$AI_MEMORY_LOG` (default
/// `info`), until the guard drops.
pub fn init(path: &Path) -> Result<DefaultGuard> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .with_env_var("AI_MEMORY_LOG")
        .from_env_lossy();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(open(path)?)
        .with_ansi(false)
        .with_env_filter(filter)
        .finish();
    Ok(tracing::subscriber::set_default(subscriber))
}

/// Starts this program with what `args` sets, in the background: no stdin or stdout, stderr
/// to the log at `log`, in its own process group. It holds none of the caller's pipes.
pub fn spawn(log: &Path, args: impl FnOnce(&mut Command) -> &mut Command) -> Result<()> {
    let exe = std::env::current_exe().context("Cannot find this program to start a summariser.")?;
    let mut cmd = Command::new(exe);
    args(&mut cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(open(log)?)
        .process_group(0)
        .spawn()
        .context("Cannot start a summariser.")?;
    Ok(())
}

/// `d` in seconds, to the millisecond.
pub fn secs(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e3).round() / 1e3
}

/// `usd` US dollars, to the millionth.
pub fn usd(usd: f64) -> f64 {
    (usd * 1e6).round() / 1e6
}

/// Logs event `$msg` at `$level` with `$fields`, then what the requests of `$spend`, a
/// [`Spend`](crate::memory::Spend), took: all of them, and the retries among them.
macro_rules! spend {
    ($level:ident, $spend:expr, { $($fields:tt)+ }, $msg:literal) => {{
        let s: $crate::memory::Spend = $spend;
        ::tracing::$level!(
            $($fields)+,
            requests = s.requests,
            retries = s.retries,
            tokens = s.all.tokens(),
            thinking = s.all.thinking,
            cost = $crate::memory::log::usd(s.all.cost),
            retry_tokens = s.retried.tokens(),
            retry_cost = $crate::memory::log::usd(s.retried.cost),
            $msg
        )
    }};
}
pub(crate) use spend;
