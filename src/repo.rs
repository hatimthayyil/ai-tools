use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use gix::{
    ObjectId,
    bstr::{BStr, ByteSlice},
    index::{entry::Stat, fs::Metadata},
    objs::tree::EntryKind,
};
use ignore::WalkBuilder;

use crate::file::{self, AtPath};

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
    if !same {
        let me = repo.committer().context("No committer.")??;
        repo.commit_as(me, me, "HEAD", msg, tree, parent)?;
    }
    let mut index = repo.index_from_tree(&tree)?;
    for (e, path) in index.entries_mut_with_paths() {
        if let Some(s) = stats.get(&path_str(path)) {
            e.stat = *s;
        }
    }
    index.write(Default::default())?;
    Ok(())
}

fn path_str(p: &BStr) -> String {
    p.to_str_lossy().into_owned()
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
