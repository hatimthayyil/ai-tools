//! The processes that could write to a folder, read from `/proc`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use regex::Regex;

use crate::file::pretty;

/// A process, by its id and first four arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub args: Vec<String>,
}

/// The processes, this one aside, that could write to `paths`: those whose first four arguments
/// `harness` matches, and those with a file or their working folder at or under one of them.
/// `proc` is where processes are listed; without one, no folder can be shown idle.
pub fn blockers(proc: &Path, harness: &Regex, paths: &[PathBuf]) -> Result<Vec<Proc>> {
    if !proc.join("self").exists() {
        bail!(
            "{} lists no processes, so nothing can be shown idle; nothing moved.",
            pretty(proc)
        );
    }
    let me = std::process::id();
    let mut found = Vec::new();
    let Ok(dir) = fs::read_dir(proc) else {
        bail!("{}: unreadable; nothing moved.", pretty(proc));
    };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == me {
            continue;
        }
        // A process that ends while it is read is no longer a writer.
        let Ok(cmdline) = fs::read(e.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = cmdline
            .split(|&b| b == 0)
            .filter(|a| !a.is_empty())
            .take(4)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        if args.iter().any(|a| harness.is_match(a)) || opens(&e.path(), paths) {
            found.push(Proc { pid, args });
        }
    }
    found.sort_by_key(|p| p.pid);
    Ok(found)
}

/// Whether the process at `dir` has a file, or its working folder, at or under one of `paths`.
/// Another user's process, whose files can't be read, counts as having none there.
fn opens(dir: &Path, paths: &[PathBuf]) -> bool {
    let under = |p: &Path| paths.iter().any(|f| p.starts_with(f));
    if fs::read_link(dir.join("cwd")).is_ok_and(|c| under(&c)) {
        return true;
    }
    let Ok(fds) = fs::read_dir(dir.join("fd")) else {
        return false;
    };
    fds.flatten()
        .any(|fd| fs::read_link(fd.path()).is_ok_and(|t| under(&t)))
}
