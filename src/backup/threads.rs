//! `ai backup threads`: where the bytes every thread in `~/ai` read of harness files are.
//!
//! A thread keeps no copy of what it read of a session's file, only what finds it again
//! ([`Want`](super::read::Want)). The file holds those bytes while it begins with them; once
//! it does not, a repository must, or they are lost.

use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::Result;

use super::{host, read::Backup};
use crate::{
    agent::{self, Version},
    config::Homes,
    file::{human, pretty},
    memory::plural,
};

/// Where the bytes a thread read of a file are, from best to worst.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// A repository holds them.
    Held,
    /// The file still holds them, and this machine's newest snapshot not yet.
    Pending,
    /// The file still holds them, and no local repository has been made.
    Bare,
    /// They were read on another machine, and no repository here holds them.
    Away,
    /// The file no longer holds them, and no repository does.
    Lost,
}

/// The state of each version of `versions`, one thread's, in their order: a file's newest
/// version on this machine is compared with the file, every byte; any other is read back from
/// the repositories of `backup`.
fn states(versions: &[Version], host: &str, backup: &mut Option<Backup>) -> Result<Vec<State>> {
    let mut newest: HashMap<(&str, &Path), usize> = HashMap::new();
    for (i, v) in versions.iter().enumerate() {
        newest.insert((&v.read.host, &v.read.path), i);
    }
    let mut live = Vec::new();
    let mut states = Vec::with_capacity(versions.len());
    for (i, v) in versions.iter().enumerate() {
        let here = v.read.host == host;
        let state = if v.read.len == 0 {
            State::Held
        } else if here
            && newest[&(v.read.host.as_str(), v.read.path.as_path())] == i
            && v.read.live()?
        {
            live.push(i);
            State::Bare
        } else {
            let held = match backup {
                Some(b) => b.fetch(&v.read)?.is_some(),
                None => false,
            };
            match (held, here) {
                (true, _) => State::Held,
                (false, true) => State::Lost,
                (false, false) => State::Away,
            }
        };
        states.push(state);
    }
    if let Some(b) = backup.as_mut().filter(|b| b.has_local()) {
        let paths: Vec<PathBuf> = live
            .iter()
            .map(|&i| versions[i].read.path.clone())
            .collect();
        let sizes = b.newest(&paths)?;
        for i in live {
            let v = &versions[i].read;
            states[i] = match sizes.get(&v.path) {
                Some(&size) if size >= v.len => State::Held,
                _ => State::Pending,
            };
        }
    }
    Ok(states)
}

/// Prints how many files the threads in `homes`' repository read are backed up and how many
/// are not, then each read on another machine that no repository here holds, on `out`; each
/// file whose bytes are lost and each thread whose records can't be read go to `err`, and fail
/// the check. A file counts once for each thread that read it, as its version that stands worst.
pub fn check(homes: &Homes, out: &mut dyn Write, err: &mut dyn Write) -> Result<ExitCode> {
    let host = host()?;
    let threads = agent::everywhere(homes)?;
    let mut backup = Backup::open(homes, &host)?;
    let mut failed = false;
    let mut worst: BTreeMap<(&str, &Path), (State, &Version)> = BTreeMap::new();
    for (address, versions) in &threads {
        let versions = match versions {
            Ok(v) => v,
            Err(e) => {
                writeln!(err, "{e:#}")?;
                failed = true;
                continue;
            }
        };
        for (v, s) in versions.iter().zip(states(versions, &host, &mut backup)?) {
            let w = worst.entry((address, &v.read.path)).or_insert((s, v));
            if s > w.0 {
                *w = (s, v);
            }
        }
    }
    if worst.is_empty() {
        writeln!(out, "No thread in {} has read a file.", pretty(&homes.repo))?;
    } else {
        let count = |s| worst.values().filter(|(w, _)| *w == s).count();
        let mut parts = vec![format!("{} backed up", count(State::Held))];
        for (s, what) in [
            (State::Pending, "not yet backed up"),
            (State::Bare, "with no backup"),
            (State::Away, "on another machine"),
            (State::Lost, "lost"),
        ] {
            if count(s) > 0 {
                parts.push(format!("{} {what}", count(s)));
            }
        }
        writeln!(
            out,
            "Threads in {} read {}: {}.",
            pretty(&homes.repo),
            plural(worst.len() as u64, "file"),
            parts.join(", ")
        )?;
        if count(State::Bare) > 0 {
            writeln!(out, "No repository yet: `ai backup run` makes it.")?;
        }
    }
    for ((address, path), (s, v)) in &worst {
        let read = human(v.read.len);
        match s {
            State::Away => writeln!(
                out,
                "  {}: {address} read {read} of it on {}, and no repository here holds them.",
                path.display(),
                v.read.host
            )?,
            State::Lost => {
                writeln!(
                    err,
                    "{}: the {read} {address} read of it are lost: the file no longer holds \
                     them, and no repository does.",
                    pretty(path)
                )?;
                failed = true;
            }
            State::Held | State::Pending | State::Bare => {}
        }
    }
    Ok(match failed {
        true => ExitCode::FAILURE,
        false => ExitCode::SUCCESS,
    })
}
