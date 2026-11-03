use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use gix::{
    ObjectId,
    bstr::{BStr, ByteSlice},
    index::{entry::Stat, fs::Metadata},
    objs::tree::EntryKind,
};
use ignore::WalkBuilder;

use crate::file::{self, AtPath, pretty};

const GIT: &str = ".git";
const LOCK: &str = "ai-commit.lock";

pub fn exists(dir: &Path) -> bool {
    git_dir(dir).is_dir()
}

/// The git directory of the repository at `dir`.
pub fn git_dir(dir: &Path) -> PathBuf {
    dir.join(GIT)
}

/// The lock [`commit`] takes: holding it keeps every commit of `dir` waiting.
pub fn lock(dir: &Path) -> Result<file::Lock> {
    file::lock(&git_dir(dir).join(LOCK))
}

/// Makes `dir` a git repository unless it is one. `.git` is made beside `dir` and renamed in,
/// so parallel callers never see half a repository.
pub fn create(dir: &Path) -> Result<()> {
    if exists(dir) {
        return Ok(());
    }
    fs::create_dir_all(dir).at(dir)?;
    let up = dir.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = tempfile::Builder::new()
        .prefix(".ai-init")
        .tempdir_in(up.unwrap_or(Path::new(".")))
        .context("Cannot create a temporary directory.")?;
    gix::init(tmp.path())?;
    let git = dir.join(GIT);
    match fs::rename(tmp.path().join(GIT), &git) {
        Err(_) if exists(dir) => Ok(()),
        r => r.at(&git),
    }
}

/// Makes `dir` a git repository unless it is one. An absent or empty directory becomes one; a
/// directory holding files but no repository is refused, since what it holds is someone's.
pub fn ensure(dir: &Path) -> Result<()> {
    if exists(dir) {
        return Ok(());
    }
    match fs::read_dir(dir).map(|mut entries| entries.next().is_some()) {
        Ok(true) => bail!(
            "{} holds files but is not a git repository. Make it one (git init {}) or move it \
             aside: ai keeps its threads there.",
            pretty(dir),
            dir.display()
        ),
        Ok(false) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).at(dir),
    }
    create(dir)
}

/// Commits what is under each of `paths`, relative to the repository at `dir`, as one commit
/// `msg` by the user's git identity, else `me`: HEAD's tree with those paths as they are on disk
/// and everything else as HEAD has it; nothing when that tree is HEAD's. Of the index, only the
/// entries under `paths` change, so what the user staged elsewhere stays staged. A file whose
/// stat matches its index entry keeps that entry's blob unread.
pub fn commit_paths(dir: &Path, paths: &[&Path], me: &str, msg: &str) -> Result<()> {
    let _lock = lock(dir)?;
    let mut repo = gix::open(dir)?;
    repo.committer_or_set_fallback(me, "ai@localhost")?;
    let mut index: gix::index::File = (**repo.index_or_empty()?).clone();
    let parent = repo.head()?.id().map(|id| id.detach());
    let base = match parent {
        Some(p) => repo.find_commit(p)?.tree_id()?.detach(),
        None => ObjectId::empty_tree(repo.object_hash()),
    };
    let mut ed = repo.edit_tree(base)?;
    let mut prefixes = Vec::new();
    let mut stats = HashMap::new();
    let mut stale = false;
    for p in paths {
        let prefix = p
            .to_str()
            .with_context(|| format!("{} is not UTF-8.", pretty(p)))?
            .trim_end_matches('/')
            .to_string();
        ed.remove(prefix.as_str())?;
        for (rel, abs) in files_under(dir, p)? {
            let stat = Metadata::from_path_no_follow(&abs)
                .at(&abs)
                .and_then(|m| Ok(Stat::from_fs(&m)?))?;
            let known = index
                .entry_by_path(rel.as_bytes().as_bstr())
                .filter(|e| e.stat == stat)
                .map(|e| e.id);
            let id = match known {
                Some(id) => id,
                None => {
                    stale = true;
                    repo.write_blob(fs::read(&abs).at(&abs)?)?.detach()
                }
            };
            ed.upsert(&rel, EntryKind::Blob, id)?;
            stats.insert(rel, stat);
        }
        prefixes.push(prefix);
    }
    let tree = ed.write()?.detach();
    let same = match parent {
        Some(p) => repo.find_commit(p)?.tree_id()? == tree,
        None => tree == ObjectId::empty_tree(repo.object_hash()),
    };
    if same && !stale {
        return Ok(());
    }
    if !same {
        let me = repo.committer().context("No committer.")??;
        repo.commit_as(me, me, "HEAD", msg, tree, parent)?;
    }
    let under = |path: &BStr| {
        prefixes.iter().any(|p| {
            path == p.as_bytes().as_bstr()
                || (path.starts_with(p.as_bytes()) && path.get(p.len()) == Some(&b'/'))
        })
    };
    index.remove_entries(|_, path, _| under(path));
    let fresh = repo.index_from_tree(&tree)?;
    for (e, path) in fresh.entries().iter().map(|e| (e, e.path(&fresh))) {
        if under(path) {
            let stat = stats.get(&path_str(path)).copied().unwrap_or(e.stat);
            index.dangerously_push_entry(stat, e.id, e.flags, e.mode, path);
        }
    }
    index.sort_entries();
    index.remove_tree();
    index.write(Default::default())?;
    Ok(())
}

/// Commits every file of `dir` its `.gitignore` keeps, as one commit `msg` by the user's git
/// identity, else `me`; nothing when the tree is HEAD's. A file whose stat matches its index
/// entry keeps that entry's blob unread.
pub fn commit(dir: &Path, me: &str, msg: &str) -> Result<()> {
    let _lock = lock(dir)?;
    let mut repo = gix::open(dir)?;
    repo.committer_or_set_fallback(me, "ai@localhost")?;
    let index = repo.index_or_empty()?;
    let mut ed = repo.edit_tree(ObjectId::empty_tree(repo.object_hash()))?;
    let mut stats = HashMap::new();
    let mut stale = false;
    for (rel, abs) in files(dir)? {
        let stat = Metadata::from_path_no_follow(&abs)
            .at(&abs)
            .and_then(|m| Ok(Stat::from_fs(&m)?))?;
        let known = index
            .entry_by_path(rel.as_bytes().as_bstr())
            .filter(|e| e.stat == stat)
            .map(|e| e.id);
        let id = match known {
            Some(id) => id,
            None => {
                stale = true;
                repo.write_blob(fs::read(&abs).at(&abs)?)?.detach()
            }
        };
        ed.upsert(&rel, EntryKind::Blob, id)?;
        stats.insert(rel, stat);
    }
    let tree = ed.write()?.detach();
    let parent = repo.head()?.id().map(|id| id.detach());
    let same = match parent {
        Some(p) => repo.find_commit(p)?.tree_id()? == tree,
        None => false,
    };
    if same && !stale {
        return Ok(());
    }
    let mut index = repo.index_from_tree(&tree)?;
    for (e, path) in index.entries_mut_with_paths() {
        if let Some(s) = stats.get(&path_str(path)) {
            e.stat = *s;
        }
    }
    // Before HEAD moves: whoever reads between the two sees the files staged, never a HEAD the
    // index lags behind.
    index.write(Default::default())?;
    if !same {
        let me = repo.committer().context("No committer.")??;
        repo.commit_as(me, me, "HEAD", msg, tree, parent)?;
    }
    Ok(())
}

fn path_str(p: &BStr) -> String {
    p.to_str_lossy().into_owned()
}

/// Every file under `rel` in the repository at `dir` that its ignore files keep, by its path
/// relative to `dir`; none if `rel` does not exist.
fn files_under(dir: &Path, rel: &Path) -> Result<Vec<(String, PathBuf)>> {
    let root = dir.join(rel);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let walker = WalkBuilder::new(&root)
        .standard_filters(false)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != GIT)
        .build();
    let mut out = Vec::new();
    for e in walker {
        let e = e?;
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = e.path().strip_prefix(dir).expect("under the walk's root");
        if let Some(rel) = rel.to_str() {
            out.push((rel.to_string(), e.path().to_path_buf()));
        }
    }
    Ok(out)
}

/// Every file under `dir` but `.git` and what its ignore files leave out, by its path relative
/// to `dir`.
fn files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let walker = WalkBuilder::new(dir)
        .standard_filters(false)
        .git_ignore(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(|e| e.depth() != 1 || e.file_name() != GIT)
        .build();
    let mut out = Vec::new();
    for e in walker {
        let e = e?;
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = e.path().strip_prefix(dir).expect("under the walk's root");
        if let Some(rel) = rel.to_str() {
            out.push((rel.to_string(), e.path().to_path_buf()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn a_commit_writes_the_index_before_head_moves() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        fs::write(dir.join("a"), "1\n").unwrap();
        commit(&dir, "ai", "one").unwrap();
        let staged = || {
            let index = gix::open(&dir).unwrap().index_or_empty().unwrap();
            let paths = index.entries().iter().map(|e| e.path(&index).to_string());
            paths.collect::<Vec<_>>()
        };
        assert_eq!(staged(), ["a"]);

        // A held lock on HEAD's branch keeps HEAD where it is.
        let branch = gix::open(&dir).unwrap().head_name().unwrap().unwrap();
        let held = git_dir(&dir).join(format!("{}.lock", branch.as_bstr()));
        fs::write(&held, "").unwrap();
        fs::write(dir.join("b"), "2\n").unwrap();
        assert!(commit(&dir, "ai", "two").is_err());
        assert_eq!(head(&dir, "a").0, "one");
        assert_eq!(staged(), ["a", "b"]);

        fs::remove_file(&held).unwrap();
        commit(&dir, "ai", "two").unwrap();
        assert_eq!(head(&dir, "b"), ("two".into(), "2\n".into()));
        assert_eq!(staged(), ["a", "b"]);
    }

    /// HEAD's subject and the blob at `path` in its tree.
    fn head(dir: &Path, path: &str) -> (String, String) {
        let repo = gix::open(dir).unwrap();
        let c = repo.head_commit().unwrap();
        let msg = c.message().unwrap().summary().to_string();
        let tree = c.tree().unwrap();
        let id = tree
            .lookup_entry_by_path(path)
            .unwrap()
            .unwrap()
            .object_id();
        let blob = repo.find_blob(id).unwrap().take_data();
        (msg, String::from_utf8(blob).unwrap())
    }

    #[test]
    fn a_commit_takes_what_changed_leaves_out_what_is_ignored_and_none_when_nothing_did() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        fs::write(dir.join(".gitignore"), "*.lock\n").unwrap();
        fs::write(dir.join("a.jsonl"), "1\n").unwrap();
        fs::write(dir.join("a.lock"), "").unwrap();
        commit(&dir, "ai", "one").unwrap();
        assert_eq!(head(&dir, "a.jsonl"), ("one".into(), "1\n".into()));

        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(dir.join("a.jsonl"))
            .unwrap();
        f.write_all(b"2\n").unwrap();
        commit(&dir, "ai", "two").unwrap();
        assert_eq!(head(&dir, "a.jsonl"), ("two".into(), "1\n2\n".into()));

        commit(&dir, "ai", "three").unwrap();
        assert_eq!(head(&dir, "a.jsonl").0, "two");
        let repo = gix::open(&dir).unwrap();
        let tree = repo.head_commit().unwrap().tree().unwrap();
        assert!(tree.lookup_entry_by_path("a.lock").unwrap().is_none());
    }
}
