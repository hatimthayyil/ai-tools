//! A session's files as a thread reads them, and what the thread records of them: each file's
//! versions, each the machine, the path, and the length and SHA-256 of the bytes read, which
//! find those bytes again in the file or in the machine's backup.

use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use ring::digest::{self, SHA256};

use crate::{
    agent::{
        claude,
        record::Pointer,
        store::{DIR, SOURCES, THREADS, check_name},
    },
    backup::read::{Backup, Want},
    config::Homes,
    file::{self, AtPath, hex, human, pretty},
    jsonl,
    project::PROJECTS,
};

/// `p` where it really sits: its folder's links resolved. While its folder doesn't exist, `p`
/// as given, made absolute.
pub fn real(p: &Path) -> PathBuf {
    let resolved = p
        .parent()
        .and_then(|d| fs::canonicalize(d).ok())
        .zip(p.file_name())
        .map(|(d, n)| d.join(n));
    resolved.unwrap_or_else(|| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
}

/// The configuration directory of the `harness` that keeps the session transcript `transcript`,
/// the path as the harness gives it; `None` when the path doesn't tell.
pub fn config(harness: &str, transcript: &Path) -> Option<PathBuf> {
    match harness {
        claude::HARNESS => claude::config_of(transcript),
        _ => None,
    }
}

/// One version of a file a thread read: the bytes it read from the file's start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub harness: String,
    pub session: String,
    /// The machine, where the file really sits on it, and the bytes' length and SHA-256.
    pub read: Want,
}

/// The versions the records `pointers` of thread `thread` make, in the order each began: a
/// record from 0 begins one, and each later record of its file says what it grew to. A version
/// of no bytes holds nothing, and the one that begins next takes its place.
pub(super) fn versions(thread: &str, pointers: Vec<Pointer>) -> Result<Vec<Version>> {
    let mut out: Vec<Version> = Vec::new();
    let mut newest: HashMap<(String, String, PathBuf), usize> = HashMap::new();
    for (i, p) in pointers.into_iter().enumerate() {
        let file = (p.harness.clone(), p.host.clone(), p.path.clone());
        let before = newest.get(&file).copied();
        if p.from != 0 {
            let Some(v) = before.map(|at| &mut out[at]) else {
                bail!(
                    "Thread {thread}: source record {} goes on from byte {} of {}, which no \
                     record before it began.",
                    i + 1,
                    p.from,
                    pretty(&p.path)
                );
            };
            if p.from != v.read.len || p.session != v.session {
                bail!(
                    "Thread {thread}: source record {} goes on from byte {} of {}, which the \
                     record before it of session {} ends at {}.",
                    i + 1,
                    p.from,
                    pretty(&p.path),
                    v.session,
                    v.read.len
                );
            }
            v.read.len = p.len;
            v.read.sha256 = p.sha256;
            v.read.at = p.at;
            continue;
        }
        let version = Version {
            harness: p.harness,
            session: p.session,
            read: Want {
                host: p.host,
                path: p.path,
                len: p.len,
                sha256: p.sha256,
                at: p.at,
            },
        };
        match before.filter(|&at| out[at].read.len == 0) {
            Some(at) => out[at] = version,
            None => {
                newest.insert(file, out.len());
                out.push(version);
            }
        }
    }
    Ok(out)
}

/// The versions the source records `sources`, a `sources.jsonl`'s bytes, of thread `thread`
/// make.
pub(super) fn decode(thread: &str, sources: &[u8]) -> Result<Vec<Version>> {
    let mut pointers = Vec::new();
    for (i, l) in jsonl::lines(sources).enumerate() {
        pointers.push(
            jsonl::decode(l)
                .with_context(|| format!("Thread {thread}: source record {} is corrupt", i + 1))?,
        );
    }
    versions(thread, pointers)
}

/// Whether `bytes`, a file from its start, begin with what `old` tells of, and the SHA-256 of
/// all of them, each byte hashed once.
pub(super) fn hashes(bytes: &[u8], old: Option<&Want>) -> (bool, String) {
    let mut all = digest::Context::new(&SHA256);
    let kept = old
        .and_then(|w| usize::try_from(w.len).ok())
        .filter(|&n| n <= bytes.len());
    let begins = match (old, kept) {
        (Some(w), Some(n)) => {
            all.update(&bytes[..n]);
            let begins = hex(all.clone().finish().as_ref()) == w.sha256;
            all.update(&bytes[n..]);
            begins
        }
        _ => {
            all.update(bytes);
            false
        }
    };
    (begins, hex(all.finish().as_ref()))
}

/// The files of `versions`, one session's on the machine `host`, that are gone from it, put
/// back from the backup: each as the newest snapshot that begins with what the thread read of
/// it holds it, whole. No file that exists is written. Returns how many it put back; an error
/// saying why if a file that is gone can't be.
pub fn restore(homes: &Homes, host: &str, versions: &[&Version]) -> Result<usize> {
    let mut newest: BTreeMap<&Path, &Version> = BTreeMap::new();
    for v in versions.iter().filter(|v| v.read.host == host) {
        newest.insert(&v.read.path, v);
    }
    let gone: Vec<&Version> = newest
        .into_values()
        .filter(|v| matches!(fs::symlink_metadata(&v.read.path), Err(e) if e.kind() == io::ErrorKind::NotFound))
        .collect();
    if gone.is_empty() {
        return Ok(0);
    }
    let Some(mut backup) = Backup::open(homes, host)? else {
        bail!("there is no backup to bring it back from: `ai backup run` makes one");
    };
    let mut files = Vec::new();
    for v in &gone {
        match backup.fetch(&v.read)? {
            Some((bytes, _)) => files.push((&v.read.path, bytes)),
            None => bail!(
                "the backup does not hold the {} this thread read of {}",
                human(v.read.len),
                pretty(&v.read.path)
            ),
        }
    }
    for (path, bytes) in &files {
        let dir = path.parent().context("A transcript needs a folder.")?;
        fs::create_dir_all(dir).at(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir).at(dir)?;
        io::Write::write_all(&mut tmp, bytes).at(tmp.path())?;
        tmp.as_file().sync_all().at(tmp.path())?;
        let (_, at) = tmp.keep().map_err(|e| e.error).at(dir)?;
        if let Err(e) = file::rename_new(&at, path) {
            let _ = fs::remove_file(&at);
            return Err(e).at(path);
        }
    }
    Ok(files.len())
}

/// The versions of every thread in `homes`' repository, by its agent's address: every project's
/// folder under `~/ai/projects/`, every thread of its agents. A thread whose records can't be
/// read has the error in their place.
pub fn everywhere(homes: &Homes) -> Result<Vec<(String, Result<Vec<Version>>)>> {
    let mut out = Vec::new();
    for (project, dir) in folders(&homes.repo.join(PROJECTS))? {
        for (name, thread) in folders(&dir.join(DIR).join(THREADS))? {
            if check_name(&name).is_err() {
                continue;
            }
            let address = format!("{project}.{name}");
            let file = thread.join(SOURCES);
            let versions = jsonl::read(&file).and_then(|bytes| decode(&address, &bytes));
            out.push((address, versions));
        }
    }
    Ok(out)
}

/// The folders in `dir`, by name; none if it doesn't exist.
fn folders(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let entries = match fs::read_dir(dir) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).at(dir),
    };
    let mut out = BTreeMap::new();
    for e in entries {
        let e = e.at(dir)?;
        if e.file_type().at(&e.path())?.is_dir()
            && let Some(n) = e.file_name().to_str()
        {
            out.insert(n.to_owned(), e.path());
        }
    }
    Ok(out.into_iter().collect())
}

pub(super) fn capture(path: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let parent = path
        .parent()
        .context("A transcript needs a parent directory.")?;
    let mut files = BTreeMap::new();
    capture_at(path, parent, &mut files, false)?;
    let companion = path.with_extension("");
    if companion != path {
        capture_at(&companion, parent, &mut files, true)?;
    }
    Ok(files)
}

fn capture_at(
    path: &Path,
    parent: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
    allow_directory: bool,
) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot inspect {}.", path.display()));
        }
    };
    if meta.is_symlink() {
        bail!(
            "A session's file must be a regular file, not a symbolic link: {}.",
            path.display()
        );
    }
    if meta.is_dir() {
        if !allow_directory {
            bail!("A transcript must be a regular file: {}.", path.display());
        }
        for entry in
            fs::read_dir(path).with_context(|| format!("Cannot read {}.", path.display()))?
        {
            capture_at(&entry?.path(), parent, files, true)?;
        }
    } else if meta.is_file() {
        let relative = path.strip_prefix(parent)?.to_path_buf();
        relative
            .to_str()
            .context("A source file name is not UTF-8.")?;
        let bytes = fs::read(path).with_context(|| format!("Cannot read {}.", path.display()))?;
        files.insert(relative, bytes);
    } else {
        bail!(
            "A session's file must be a regular file: {}.",
            path.display()
        );
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn source_capture_refuses_links_into_external_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let transcript = dir.path().join("s-1.jsonl");
        fs::write(&transcript, "").unwrap();
        let companion = transcript.with_extension("");
        fs::create_dir(&companion).unwrap();
        let outside = dir.path().join("external");
        fs::write(&outside, "outside session").unwrap();
        std::os::unix::fs::symlink(&outside, companion.join("linked-file")).unwrap();

        let error = capture(&transcript).unwrap_err();

        assert!(error.to_string().contains("not a symbolic link"));
    }

    #[test]
    fn a_directory_cannot_be_used_as_the_main_transcript() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("unrelated.jsonl"), "outside session").unwrap();

        let error = capture(dir.path()).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("A transcript must be a regular file")
        );
    }
}
