//! Reading back the bytes a reader took of a harness's file, as a thread records what it read:
//! the machine, the file's real path, the length and SHA-256 of the bytes from its start, and
//! when it read them ([`Want`]).
//!
//! The file itself holds them while it still begins with them. Once it does not, the backup
//! does: the local repository, then the remote one. No record names a snapshot, since a
//! snapshot copied to the remote gets another id there. The time finds it: the file held the
//! bytes when they were read, and holds them from when it was written to there until it is
//! rewritten or gone, so a repository holds them if and only if the machine's last snapshot
//! before that time or its first one after does ([`probes`]). A search looks in those two and a
//! few later ones, never in every snapshot.

use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

use anyhow::Result;
use jiff::Timestamp;

use super::{
    Config, STAGING, db, local, made, remote,
    restic::{self, In, Repo},
};
use crate::{
    config::Homes,
    file::{AtPath, sha256},
};

/// How many paths one `restic find` is asked for.
const BATCH: usize = 200;

/// The first `len` bytes of the file at `path`, where it really sits on the machine `host`,
/// which hash to `sha256`, as they were read at `at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub host: String,
    pub path: PathBuf,
    pub len: u64,
    pub sha256: String,
    pub at: Timestamp,
}

impl Want {
    /// Whether `bytes`, a file from its start, begin with what is wanted.
    pub fn begins(&self, bytes: &[u8]) -> bool {
        usize::try_from(self.len)
            .ok()
            .and_then(|n| bytes.get(..n))
            .is_some_and(|b| sha256(b) == self.sha256)
    }

    /// Whether the file at its path on this machine is at least as long as what is wanted:
    /// the quick sign that the file still holds it. A file that is gone is not.
    pub fn stands(&self) -> Result<bool> {
        match std::fs::metadata(&self.path) {
            Ok(m) => Ok(m.is_file() && m.len() >= self.len),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).at(&self.path),
        }
    }

    /// Whether the file at its path on this machine still begins with what is wanted, every
    /// byte read. A file that is gone does not.
    pub fn live(&self) -> Result<bool> {
        let f = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).at(&self.path),
        };
        if f.metadata().at(&self.path)?.len() < self.len {
            return Ok(false);
        }
        let mut bytes = Vec::new();
        f.take(self.len).read_to_end(&mut bytes).at(&self.path)?;
        Ok(self.begins(&bytes))
    }
}

/// Which repository bytes were read back from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum From {
    Local,
    Remote,
}

/// The snapshots to look in for bytes read at `at`, of `snapshots`, a machine's by time: the
/// last made before `at` and the first made after it, one of which holds the bytes if any
/// does; then ones made later, each twice as far on as the one before, and the newest, which
/// hold the file as it grew since.
fn probes(snapshots: &[(Timestamp, String)], at: Timestamp) -> Vec<&str> {
    let after = snapshots.partition_point(|(made, _)| *made <= at);
    let mut picked: Vec<usize> = after.checked_sub(1).into_iter().collect();
    let mut step = 1;
    let mut i = after;
    while i < snapshots.len() {
        picked.push(i);
        i += step;
        step *= 2;
    }
    picked.extend(snapshots.len().checked_sub(1));
    picked.dedup();
    picked.iter().map(|&i| snapshots[i].1.as_str()).collect()
}

/// The backup's repositories, each opened when first read: opening one runs the commands that
/// print its password and its variables.
pub struct Backup {
    homes: Homes,
    cfg: Config,
    host: String,
    local: Option<Repo>,
    remote: Option<Repo>,
    /// Each machine's snapshots in a repository, listed once.
    snapshots: HashMap<(From, String), Vec<(Timestamp, String)>>,
}

impl Backup {
    /// The backup `homes`' settings name, read as the machine `host`. `None` while there is
    /// nothing to read: no local repository made yet, and no remote one set.
    pub fn open(homes: &Homes, host: &str) -> Result<Option<Backup>> {
        let cfg = Config::load(homes)?;
        if !made(&cfg) && cfg.remote.is_none() {
            return Ok(None);
        }
        Ok(Some(Backup {
            local: None,
            remote: None,
            snapshots: HashMap::new(),
            homes: homes.clone(),
            host: host.to_owned(),
            cfg,
        }))
    }

    /// Whether a local repository has been made.
    pub fn has_local(&self) -> bool {
        made(&self.cfg)
    }

    fn repo(&mut self, from: From) -> Result<Option<&Repo>> {
        match from {
            From::Local => {
                if self.local.is_none() && made(&self.cfg) {
                    self.local = Some(local(&self.homes, &self.cfg)?);
                }
                Ok(self.local.as_ref())
            }
            From::Remote => {
                if self.remote.is_none()
                    && let Some(r) = &self.cfg.remote
                {
                    self.remote = Some(remote(r)?);
                }
                Ok(self.remote.as_ref())
            }
        }
    }

    /// The paths a snapshot may hold the file at `want.path` under: the path itself, and for
    /// this machine's, the path of a database's copy.
    fn paths(&self, want: &Want) -> Vec<PathBuf> {
        let mut paths = vec![want.path.clone()];
        if want.host == self.host {
            paths.push(db::staged(&self.homes.machine.join(STAGING), &want.path));
        }
        paths
    }

    /// The size of each of `paths` in this machine's newest snapshot of the local repository;
    /// a path the snapshot lacks has none. Empty without a local repository.
    pub fn newest(&mut self, paths: &[PathBuf]) -> Result<HashMap<PathBuf, u64>> {
        let host = self.host.clone();
        let Some(repo) = self.repo(From::Local)? else {
            return Ok(HashMap::new());
        };
        let mut sizes = HashMap::new();
        for batch in paths.chunks(BATCH) {
            restic::find(repo, &host, batch, In::Newest, &mut |f| {
                sizes.insert(f.path, f.size);
            })?;
        }
        Ok(sizes)
    }

    /// The file `want` tells of, whole, as the newest snapshot looked in ([`probes`]) whose
    /// file begins with the wanted bytes holds it, and the repository it came from: the local
    /// one, else the remote. `None` if neither holds them.
    pub fn fetch(&mut self, want: &Want) -> Result<Option<(Vec<u8>, From)>> {
        let paths = self.paths(want);
        for from in [From::Local, From::Remote] {
            if self.repo(from)?.is_none() {
                continue;
            }
            let listed = (from, want.host.clone());
            if !self.snapshots.contains_key(&listed) {
                let repo = self.repo(from)?.expect("opened above");
                let all = restic::snapshots(repo, &want.host)?;
                self.snapshots.insert(listed.clone(), all);
            }
            let within = probes(&self.snapshots[&listed], want.at);
            let repo = match from {
                From::Local => self.local.as_ref(),
                From::Remote => self.remote.as_ref(),
            }
            .expect("opened above");
            // Each state of the file a snapshot holds that is long enough, newest first; of the
            // snapshots that hold one state, any serves.
            let mut states: BTreeMap<_, (String, PathBuf)> = BTreeMap::new();
            restic::find(repo, &want.host, &paths, In::These(&within), &mut |f| {
                if f.size >= want.len && paths.contains(&f.path) {
                    states
                        .entry((f.written, f.size, f.path.clone()))
                        .or_insert((f.snapshot, f.path));
                }
            })?;
            for (snapshot, path) in states.values().rev() {
                let bytes = restic::dump(repo, snapshot, path)?;
                if want.begins(&bytes) {
                    return Ok(Some((bytes, from)));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_looks_in_the_snapshots_around_the_time_and_ever_fewer_after() {
        let at = |s: i64| Timestamp::from_second(1_800_000_000 + s).unwrap();
        let snapshots: Vec<(Timestamp, String)> =
            (0..40).map(|i| (at(i * 10), format!("s{i}"))).collect();
        assert_eq!(
            probes(&snapshots, at(95)),
            ["s9", "s10", "s11", "s13", "s17", "s25", "s39"]
        );
        assert_eq!(probes(&snapshots, at(90)), probes(&snapshots, at(95)));
        assert_eq!(
            probes(&snapshots, at(-5)),
            ["s0", "s1", "s3", "s7", "s15", "s31", "s39"],
            "none before"
        );
        assert_eq!(probes(&snapshots, at(1000)), ["s39"], "none after");
        assert_eq!(probes(&snapshots, at(385)), ["s38", "s39"]);
        assert_eq!(probes(&[], at(0)), Vec::<&str>::new());
    }
}
