use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use gix::{
    ObjectId,
    bstr::{BStr, ByteSlice},
    index::{entry::Stat, fs::Metadata},
    objs::tree::EntryKind,
};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::{
    error,
    file::{self, AtPath, pretty},
    git,
};

const GIT: &str = ".git";
const LOCK: &str = "ai-commit.lock";
/// In the git directory: a file for each commit [`ask`] queued and none has made yet.
const QUEUE: &str = "ai-queue";
/// In the git directory: held by the process making the commits [`ask`] queued.
const COMMITTER: &str = "ai-committer.lock";
/// In the git directory: why the last commit of the queue failed; gone once one succeeds.
const FAILED: &str = "ai-commit-failed";
/// In the git directory: the newest modification time among the loose objects [`gc`] counted when
/// it last started `git gc`, in nanoseconds since the epoch.
const GC_MARK: &str = "ai-gc";

pub fn exists(dir: &Path) -> bool {
    git_dir(dir).is_dir()
}

/// The git directory of the repository at `dir`.
pub fn git_dir(dir: &Path) -> PathBuf {
    dir.join(GIT)
}

/// The lock [`commit`] takes: holding it keeps every commit `ai` makes in `dir` waiting. git
/// knows nothing of it; git's own locks keep `ai` and git apart.
pub fn lock(dir: &Path) -> Result<file::Lock> {
    file::lock(&git_dir(dir).join(LOCK))
}

/// A commit [`ask`] queued: what is under `path`, relative to the repository, as `msg`.
#[derive(Serialize, Deserialize)]
struct Ask {
    path: String,
    msg: String,
}

/// Asks for what is under `rel`, relative to the repository at `dir`, to be committed as `msg`,
/// by queueing it for [`commit_asked`] in the git directory. Each ask is a file of its own,
/// named by when it was made and written whole before one rename shows it, so none is read half
/// written and none replaces another.
pub fn ask(dir: &Path, rel: &Path, msg: &str) -> Result<()> {
    let queue = git_dir(dir).join(QUEUE);
    fs::create_dir_all(&queue).at(&queue)?;
    let path = rel
        .to_str()
        .with_context(|| format!("{} is not UTF-8.", pretty(rel)))?
        .trim_end_matches('/')
        .to_string();
    let mut f = tempfile::Builder::new()
        .prefix(".")
        .tempfile_in(&queue)
        .at(&queue)?;
    let ask = Ask {
        path,
        msg: msg.into(),
    };
    serde_json::to_writer(&mut f, &ask)?;
    let made = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let suffix = f.path().file_name().and_then(|n| n.to_str()).unwrap_or("");
    let to = queue.join(format!("{made:020}{suffix}"));
    f.persist_noclobber(&to).map_err(|e| e.error).at(&to)?;
    Ok(())
}

/// The files of the asks queued in the repository at `dir`, in no order: those [`ask`] has
/// shown, not one still being written.
fn queued(dir: &Path) -> Result<Vec<PathBuf>> {
    let queue = git_dir(dir).join(QUEUE);
    let mut found = Vec::new();
    for e in read_dir_if(&queue)? {
        let p = e.at(&queue)?.path();
        if p.file_name()
            .is_some_and(|n| !n.as_encoded_bytes().starts_with(b"."))
        {
            found.push(p);
        }
    }
    Ok(found)
}

/// The asks queued in the repository at `dir`, oldest first, each with its file. One whose file
/// a committer removed meanwhile is left out: it is committed.
fn asked(dir: &Path) -> Result<Vec<(PathBuf, Ask)>> {
    let mut found = Vec::new();
    for p in queued(dir)? {
        let bytes = match fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).at(&p),
        };
        let ask = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not an ask ai wrote.", pretty(&p)))?;
        found.push((p, ask));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// Whether a process holds the lock [`commit_asked`] commits under: one that will look at the
/// queue again before it ends.
pub fn committing(dir: &Path) -> Result<bool> {
    file::held(&git_dir(dir).join(COMMITTER))
}

/// Makes the commits [`ask`] queued in the repository at `dir`, by the user's git identity,
/// else `me`: every ask found as one [`commit_paths`] of their paths, its message theirs in the
/// order they were asked, then again until none is left; how many asks it committed. It commits
/// holding the committer lock, waiting for another committer to let go of it first. An ask's
/// file goes only once its commit is made, so a committer cut short leaves its asks for the
/// next.
///
/// An ask queued while the lock is held may start no committer of its own ([`committing`]), so
/// once this lets go of the lock, it looks at the queue again. A failure is recorded for
/// [`failed`] and stops it, leaving what it failed on queued; a success clears the record.
pub fn commit_asked(dir: &Path, me: &str) -> Result<usize> {
    let mut n = 0;
    loop {
        let held = file::lock(&git_dir(dir).join(COMMITTER))?;
        let done = commit_queue(dir, me);
        record(dir, done.as_ref().err())?;
        n += done?;
        drop(held);
        if queued(dir)?.is_empty() {
            return Ok(n);
        }
    }
}

/// [`commit_asked`]'s commits, under its lock.
fn commit_queue(dir: &Path, me: &str) -> Result<usize> {
    let mut n = 0;
    loop {
        let asks = asked(dir)?;
        if asks.is_empty() {
            return Ok(n);
        }
        let mut paths: Vec<&str> = Vec::new();
        let mut msgs: Vec<&str> = Vec::new();
        for (_, a) in &asks {
            if !paths.contains(&a.path.as_str()) {
                paths.push(&a.path);
            }
            if !msgs.contains(&a.msg.as_str()) {
                msgs.push(&a.msg);
            }
        }
        let rels: Vec<&Path> = paths.iter().map(Path::new).collect();
        commit_paths(dir, &rels, me, &msgs.join("; ")).with_context(|| {
            let names: Vec<String> = paths.iter().map(|p| pretty(&dir.join(p))).collect();
            format!("Cannot commit {}", names.join(", "))
        })?;
        for (p, _) in &asks {
            fs::remove_file(p).at(p)?;
        }
        n += asks.len();
    }
}

/// Records why [`commit_asked`] failed for [`failed`]; with no error, clears the record.
fn record(dir: &Path, failure: Option<&anyhow::Error>) -> Result<()> {
    let path = git_dir(dir).join(FAILED);
    let Some(e) = failure else {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e).at(&path),
            _ => Ok(()),
        };
    };
    let mut f = tempfile::NamedTempFile::new_in(git_dir(dir)).at(&git_dir(dir))?;
    writeln!(f, "{}", error::line(e)).at(f.path())?;
    f.persist(&path).map_err(|e| e.error).at(&path)?;
    Ok(())
}

/// Why the last commit of the queue of the repository at `dir` failed, unless one has
/// succeeded since: what it asked for waits there, uncommitted.
pub fn failed(dir: &Path) -> Result<Option<String>> {
    let path = git_dir(dir).join(FAILED);
    match fs::read_to_string(&path) {
        Ok(why) => Ok(Some(why.trim_end().to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).at(&path),
    }
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

/// Whether [`ensure`] would make `dir` a git repository or find it one: an error for a directory
/// holding files but no repository, since what it holds is someone's.
pub fn ensurable(dir: &Path) -> Result<()> {
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
        Ok(false) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).at(dir),
    }
}

/// Makes `dir` a git repository unless it is one. An absent or empty directory becomes one; a
/// directory holding files but no repository is refused ([`ensurable`]).
pub fn ensure(dir: &Path) -> Result<()> {
    ensurable(dir)?;
    create(dir)
}

/// What [`gc`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Gc {
    /// The loose objects were within the limit, so nothing started.
    Within,
    /// `git gc` started.
    Started,
    /// No `git` program could be started.
    NoGit,
}

/// The variables that would point git at objects or an index other than the repository's own.
const GIT_ENV: [&str; 5] = [
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// `git` run on the git directory `git_dir`, whatever repository its environment names.
pub fn git(git_dir: &Path) -> Command {
    let mut c = Command::new("git");
    for k in GIT_ENV {
        c.env_remove(k);
    }
    c.arg("--git-dir").arg(git_dir);
    c
}

/// Starts `git gc` on the repository at `dir` once the loose objects written since it last
/// started one pass `limit` bytes, and returns without waiting for it. gix writes each object
/// whole and never packs, so a file that grows is stored whole again by every commit until
/// `git gc` packs it against its earlier versions. git's own `gc --auto` counts loose objects,
/// not their bytes, so it would wait for hundreds of commits. The gc keeps git's default grace
/// before pruning, so objects a commit writes while it runs stay; git's `gc.pid` keeps a second
/// gc from starting meanwhile.
///
/// Objects a gc leaves loose (unreferenced ones a git without cruft packs keeps until they age
/// out, or all of them when it was killed) are older than its start, so they count for nothing
/// and cannot start one gc after another; the next gc takes them with it.
pub fn gc(dir: &Path, limit: u64) -> Result<Gc> {
    let loose = loose(dir, gc_mark(dir)?)?;
    if loose.bytes <= limit {
        return Ok(Gc::Within);
    }
    let child = git(&git_dir(dir))
        .args(["gc", "--quiet"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match child {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
            if let Some(newest) = loose.newest {
                set_gc_mark(dir, newest)?;
            }
            Ok(Gc::Started)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Gc::NoGit),
        Err(e) => Err(e).context("Cannot start git gc."),
    }
}

/// The bytes the repository at `dir` holds in loose objects. A gc running meanwhile may remove
/// what this counts; what it removed counts as nothing.
pub fn loose_bytes(dir: &Path) -> Result<u64> {
    Ok(loose(dir, None)?.bytes)
}

/// What [`loose`] found.
struct Loose {
    /// The bytes of the loose objects modified after the given time, of all of them without one.
    bytes: u64,
    /// The newest modification time among all the loose objects.
    newest: Option<SystemTime>,
}

/// The loose objects of the repository at `dir`, counting those modified after `since`.
fn loose(dir: &Path, since: Option<SystemTime>) -> Result<Loose> {
    let objects = git_dir(dir).join("objects");
    let mut found = Loose {
        bytes: 0,
        newest: None,
    };
    for e in read_dir_if(&objects)? {
        let e = e.at(&objects)?;
        let hex = e
            .file_name()
            .to_str()
            .is_some_and(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_hexdigit()));
        if !hex {
            continue;
        }
        let sub = e.path();
        for o in read_dir_if(&sub)? {
            let o = o.at(&sub)?;
            let m = match o.metadata() {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).at(&o.path()),
            };
            let at = m.modified().at(&o.path())?;
            if since.is_none_or(|s| at > s) {
                found.bytes += m.len();
            }
            found.newest = found.newest.max(Some(at));
        }
    }
    Ok(found)
}

/// The time [`gc`] last recorded for the repository at `dir`. None when it recorded none, and
/// when the record cannot be a time it wrote (not a number, or later than now, as after the
/// clock was set back): every loose object then counts, and the next gc records anew.
fn gc_mark(dir: &Path) -> Result<Option<SystemTime>> {
    let path = git_dir(dir).join(GC_MARK);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(None),
        Err(e) => return Err(e).at(&path),
    };
    Ok(text
        .trim()
        .parse()
        .ok()
        .and_then(|n| UNIX_EPOCH.checked_add(Duration::from_nanos(n)))
        .filter(|t| *t <= SystemTime::now()))
}

/// Records `newest` as the time [`gc_mark`] returns for the repository at `dir`.
fn set_gc_mark(dir: &Path, newest: SystemTime) -> Result<()> {
    let path = git_dir(dir).join(GC_MARK);
    let nanos = newest
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    fs::write(&path, format!("{nanos}\n")).at(&path)
}

/// The entries of `dir`; none if it does not exist.
fn read_dir_if(dir: &Path) -> Result<Vec<io::Result<fs::DirEntry>>> {
    match fs::read_dir(dir) {
        Ok(d) => Ok(d.collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).at(dir),
    }
}

/// Commits what is under each of `paths`, relative to the repository at `dir`, as one commit
/// `msg` by the user's git identity, else `me`: HEAD's tree with those paths as they are on disk
/// and everything else as HEAD has it; nothing when that tree is HEAD's. Of the index, only the
/// entries under `paths` change, so what the user staged elsewhere stays staged. A file whose
/// stat matches its index entry keeps that entry's blob unread. It commits as [`commit_with`].
pub fn commit_paths(dir: &Path, paths: &[&Path], me: &str, msg: &str) -> Result<()> {
    commit_with(dir, me, msg, |repo, index, parent| {
        let mut index = index.clone();
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
                let (id, stat) = blob(repo, &index, &rel, &abs, &mut stale)?;
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
            return Ok(None);
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
        Ok(Some(Staged { tree, index, same }))
    })
}

/// Commits every file of `dir` its `.gitignore` keeps, as one commit `msg` by the user's git
/// identity, else `me`; nothing when the tree is HEAD's. A file whose stat matches its index
/// entry keeps that entry's blob unread. It commits as [`commit_with`].
pub fn commit(dir: &Path, me: &str, msg: &str) -> Result<()> {
    commit_with(dir, me, msg, |repo, index, parent| {
        let mut ed = repo.edit_tree(ObjectId::empty_tree(repo.object_hash()))?;
        let mut stats = HashMap::new();
        let mut stale = false;
        for (rel, abs) in files(dir)? {
            let (id, stat) = blob(repo, index, &rel, &abs, &mut stale)?;
            ed.upsert(&rel, EntryKind::Blob, id)?;
            stats.insert(rel, stat);
        }
        let tree = ed.write()?.detach();
        let same = match parent {
            Some(p) => repo.find_commit(p)?.tree_id()? == tree,
            None => false,
        };
        if same && !stale {
            return Ok(None);
        }
        let mut index = repo.index_from_tree(&tree)?;
        for (e, path) in index.entries_mut_with_paths() {
            if let Some(s) = stats.get(&path_str(path)) {
                e.stat = *s;
            }
        }
        Ok(Some(Staged { tree, index, same }))
    })
}

/// The blob of the file `abs`, at `rel` in the repository, and its stat: its index entry's when
/// the stat matches the entry's, else the file read and written, which marks the index `stale`.
fn blob(
    repo: &gix::Repository,
    index: &gix::index::File,
    rel: &str,
    abs: &Path,
    stale: &mut bool,
) -> Result<(ObjectId, Stat)> {
    let stat = Metadata::from_path_no_follow(abs)
        .at(abs)
        .and_then(|m| Ok(Stat::from_fs(&m)?))?;
    let known = index
        .entry_by_path(rel.as_bytes().as_bstr())
        .filter(|e| e.stat == stat)
        .map(|e| e.id);
    let id = match known {
        Some(id) => id,
        None => {
            *stale = true;
            repo.write_blob(fs::read(abs).at(abs)?)?.detach()
        }
    };
    Ok((id, stat))
}

/// What a commit of [`commit_with`] makes: its tree, the index to go with it, and whether the
/// tree is HEAD's, so that only the index changes.
struct Staged {
    tree: ObjectId,
    index: gix::index::File,
    same: bool,
}

/// Commits what `stage` makes of the index and HEAD's commit in the repository at `dir`, as one
/// commit `msg` by the user's git identity, else `me`, holding [`lock`]; `stage` gives `None`
/// when neither the tree nor the index would change.
///
/// It commits as `git commit -a` does, so that a git command run meanwhile loses no write and
/// sees none half made. First it looks, as `git status` does, without any of git's locks, and
/// takes none when there is nothing to commit, so a commit with nothing in it never turns a git
/// command away. Then it takes git's `index.lock` and reads the index under it, writes the new
/// index into the lock, moves HEAD as git does ([`git::advance_head`]), and only then renames the
/// lock over the index. HEAD moved meanwhile, it lets the lock go and starts again on the new
/// HEAD. A lock it cannot move HEAD past leaves the index and HEAD as they were.
fn commit_with(
    dir: &Path,
    me: &str,
    msg: &str,
    stage: impl Fn(&gix::Repository, &gix::index::File, Option<ObjectId>) -> Result<Option<Staged>>,
) -> Result<()> {
    let _lock = lock(dir)?;
    let mut repo = gix::open(dir)?;
    repo.committer_or_set_fallback(me, "ai@localhost")?;
    if stage(&repo, &read_index(&repo)?, git::head(&repo)?.commit)?.is_none() {
        return Ok(());
    }
    loop {
        let mut held = index_lock(&repo)?;
        let index = read_index(&repo)?;
        let parent = git::head(&repo)?.commit;
        let Some(s) = stage(&repo, &index, parent)? else {
            return Ok(());
        };
        let mut out = io::BufWriter::with_capacity(64 * 1024, &mut held);
        s.index.write_to(&mut out, Default::default())?;
        out.flush().at(&repo.index_path())?;
        drop(out);
        if !s.same {
            let me = repo.committer().context("No committer.")??;
            let commit = repo.new_commit_as(me, me, msg, s.tree, parent)?.id;
            if !git::advance_head(&repo, parent, commit, me, msg)? {
                continue;
            }
        }
        let path = repo.index_path();
        held.commit().map_err(|e| e.error).at(&path)?;
        return Ok(());
    }
}

/// git's lock of the index of `repo`, `index.lock`, waited for as git waits for a ref's lock:
/// an error naming it while another holds it, which ai never takes from it.
fn index_lock(repo: &gix::Repository) -> Result<gix::lock::File> {
    let path = repo.index_path();
    git::lock(repo, &path, None).map_err(|e| {
        let lock = path.with_extension("lock");
        match lock.exists() {
            true => anyhow::anyhow!(
                "git's {} is held: a git command is using {}'s index. Let it finish; if none \
                 is running, one that was cut short left the lock behind, and it can go.",
                pretty(&lock),
                pretty(repo.workdir().unwrap_or(repo.git_dir()))
            ),
            false => gix::Error::from(e).into(),
        }
    })
}

/// The index of `repo` as its file holds it now; an empty one when there is no file.
fn read_index(repo: &gix::Repository) -> Result<gix::index::File> {
    let path = repo.index_path();
    match path.exists() {
        true => Ok(repo.open_index()?),
        false => Ok(gix::index::File::from_state(
            gix::index::State::new(repo.object_hash()),
            path,
        )),
    }
}

/// HEAD's commit of the repository at `dir` when its tree holds `rel`, relative to `dir`; `None`
/// when it does not, or there is no commit.
pub fn head_holding(dir: &Path, rel: &Path) -> Result<Option<ObjectId>> {
    let repo = gix::open(dir)?;
    let Some(id) = repo.head()?.id() else {
        return Ok(None);
    };
    let rel = rel
        .to_str()
        .with_context(|| format!("{} is not UTF-8.", pretty(rel)))?;
    let tree = repo.find_commit(id)?.tree()?;
    Ok(tree.lookup_entry_by_path(rel)?.map(|_| id.detach()))
}

/// Writes the files commit `commit` of the repository at `dir` holds under the directory `rel`,
/// relative to `dir`, into the new directory `to`; `false`, with nothing written, when the
/// commit holds no such directory. `commit` is a commit's whole id.
pub fn checkout(dir: &Path, commit: &str, rel: &Path, to: &Path) -> Result<bool> {
    let repo = gix::open(dir)?;
    let Ok(id) = ObjectId::from_hex(commit.as_bytes()) else {
        bail!("'{commit}' is not a commit's id: give every digit of it.");
    };
    let tree = repo
        .find_commit(id)
        .with_context(|| format!("{} holds no commit {commit}.", pretty(dir)))?
        .tree()?;
    let path = rel
        .to_str()
        .with_context(|| format!("{} is not UTF-8.", pretty(rel)))?;
    let Some(entry) = tree
        .lookup_entry_by_path(path)?
        .filter(|e| e.mode().is_tree())
    else {
        return Ok(false);
    };
    fs::create_dir(to).at(to)?;
    for e in entry
        .object()?
        .into_tree()
        .traverse()
        .breadthfirst
        .files()?
    {
        let name = path_str(e.filepath.as_ref());
        if !e.mode.is_blob() {
            bail!("{path}/{name} in commit {commit} is not a plain file.");
        }
        let p = to.join(&name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).at(parent)?;
        }
        fs::write(&p, &repo.find_blob(e.oid)?.data).at(&p)?;
    }
    Ok(true)
}

/// Every path under `rel`, relative to the repository at `dir`, that no commit would hold: what
/// its ignore files leave out, and what is neither a file nor a directory.
pub fn left_out(dir: &Path, rel: &Path) -> Result<Vec<PathBuf>> {
    let kept: HashSet<PathBuf> = files_under(dir, rel)?
        .into_iter()
        .map(|(_, abs)| abs)
        .collect();
    let root = dir.join(rel);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for e in WalkBuilder::new(&root).standard_filters(false).build() {
        let e = e?;
        if e.file_type().is_some_and(|t| t.is_dir()) || kept.contains(e.path()) {
            continue;
        }
        out.push(e.path().to_path_buf());
    }
    out.sort();
    Ok(out)
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

    /// The lock of the branch HEAD of the repository at `dir` names.
    fn branch_lock(dir: &Path) -> PathBuf {
        let branch = gix::open(dir).unwrap().head_name().unwrap().unwrap();
        git_dir(dir).join(format!("{}.lock", branch.as_bstr()))
    }

    #[test]
    fn a_commit_that_cannot_move_head_leaves_head_and_the_index_as_they_were() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        fs::write(dir.join("a"), "1\n").unwrap();
        commit(&dir, "ai", "one").unwrap();
        assert_eq!(staged(&dir), ["a"]);

        let held = branch_lock(&dir);
        fs::write(&held, "").unwrap();
        fs::write(dir.join("b"), "2\n").unwrap();
        assert!(commit(&dir, "ai", "two").is_err());
        assert_eq!(head(&dir, "a").0, "one");
        assert_eq!(staged(&dir), ["a"]);
        assert!(!git_dir(&dir).join("index.lock").exists());
        assert!(!git_dir(&dir).join("HEAD.lock").exists());

        fs::remove_file(&held).unwrap();
        commit(&dir, "ai", "two").unwrap();
        assert_eq!(head(&dir, "b"), ("two".into(), "2\n".into()));
        assert_eq!(staged(&dir), ["a", "b"]);
    }

    #[test]
    fn a_commit_is_logged_as_git_logs_one() {
        let (_tmp, dir) = first_commit();
        fs::write(dir.join("a"), "1\n").unwrap();
        commit(&dir, "ai", "one\n\nbody").unwrap();
        let branch = gix::open(&dir).unwrap().head_name().unwrap().unwrap();
        let ids = git(&dir, &["rev-list", "HEAD"]);
        let ids: Vec<&str> = ids.lines().collect();
        let none = "0".repeat(40);
        for log in ["HEAD".to_string(), branch.to_string()] {
            let text = fs::read_to_string(git_dir(&dir).join("logs").join(&log)).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 2, "{log}: {text}");
            for (line, (from, to, why)) in lines.iter().zip([
                (none.as_str(), ids[1], "commit (initial): first"),
                (ids[1], ids[0], "commit: one"),
            ]) {
                let (moved, said) = line.split_once('\t').unwrap();
                assert_eq!(said, why, "{log}");
                let who = git(
                    &dir,
                    &["log", "-1", "--date=raw", "--format=%cn <%ce> %cd", to],
                );
                assert_eq!(moved, format!("{from} {to} {}", who.trim()), "{log}");
            }
            assert_eq!(
                git(&dir, &["reflog", "show", "--format=%gs", &log]),
                "commit: one\ncommit (initial): first\n"
            );
        }
    }

    #[test]
    fn a_commit_makes_no_reflog_where_git_makes_none_but_adds_to_one_there_is() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        git(&dir, &["config", "core.logAllRefUpdates", "false"]);
        fs::write(dir.join("a"), "1\n").unwrap();
        commit(&dir, "ai", "one").unwrap();
        let logs = git_dir(&dir).join("logs");
        assert!(!logs.exists());
        fs::create_dir(&logs).unwrap();
        fs::write(logs.join("HEAD"), "").unwrap();
        fs::write(dir.join("a"), "2\n").unwrap();
        commit(&dir, "ai", "two").unwrap();
        assert_eq!(
            git(&dir, &["reflog", "show", "--format=%gs", "HEAD"]),
            "commit: two\n"
        );
        assert!(!logs.join("refs").exists());
    }

    #[test]
    fn a_commit_on_a_detached_head_moves_head_alone() {
        let (_tmp, dir) = first_commit();
        let branch = gix::open(&dir).unwrap().head_name().unwrap().unwrap();
        let first = git(&dir, &["rev-parse", "HEAD"]);
        fs::write(git_dir(&dir).join("HEAD"), &first).unwrap();
        fs::write(dir.join("a"), "1\n").unwrap();
        commit(&dir, "ai", "one").unwrap();
        let head = fs::read_to_string(git_dir(&dir).join("HEAD")).unwrap();
        assert_ne!(head, first);
        assert_eq!(git(&dir, &["rev-parse", "HEAD~1"]), first);
        assert_eq!(git(&dir, &["rev-parse", &branch.to_string()]), first);
        assert_eq!(
            git(&dir, &["reflog", "show", "--format=%gs", "HEAD"]),
            "commit: one\ncommit (initial): first\n"
        );
    }

    #[test]
    fn a_commit_waits_for_head_s_lock_as_git_does() {
        let (_tmp, dir) = first_commit();
        let held = git_dir(&dir).join("HEAD.lock");
        fs::write(&held, "").unwrap();
        fs::write(dir.join("a"), "1\n").unwrap();
        let l = error::line(&commit(&dir, "ai", "one").unwrap_err());
        assert!(l.contains(&held.display().to_string()), "{l}");
        assert_eq!(head(&dir, "first").0, "first");
        assert_eq!(staged(&dir), ["first"]);
        fs::remove_file(&held).unwrap();
        commit(&dir, "ai", "one").unwrap();
        assert_eq!(head(&dir, "a").0, "one");
    }

    #[test]
    fn a_branch_git_packed_moves_on_from_its_packed_commit() {
        let (_tmp, dir) = first_commit();
        git(&dir, &["pack-refs", "--all", "--prune"]);
        assert!(!branch_lock(&dir).with_extension("").exists());
        fs::write(dir.join("a"), "1\n").unwrap();
        commit(&dir, "ai", "one").unwrap();
        assert_eq!(subjects(&dir), "one\nfirst\n");
    }

    /// The paths the index of the repository at `dir` holds, in its order.
    fn staged(dir: &Path) -> Vec<String> {
        let index = gix::open(dir).unwrap().index_or_empty().unwrap();
        let paths = index.entries().iter().map(|e| e.path(&index).to_string());
        paths.collect()
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

    /// `git args` in the repository at `dir`, which must succeed; its output.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(git_dir(dir))
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Waits until the repository at `dir` holds a pack, up to a minute.
    fn packed(dir: &Path) {
        let pack = git_dir(dir).join("objects/pack");
        for _ in 0..600 {
            let found = fs::read_dir(&pack).is_ok_and(|d| {
                d.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "pack"))
            });
            if found {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("no pack in {}", pack.display());
    }

    /// Appends `n` bytes of one line to `p`.
    fn grow(p: &Path, n: usize, tag: usize) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap();
        writeln!(f, "{tag} {}", "x".repeat(n)).unwrap();
    }

    #[test]
    fn gc_starts_only_once_the_loose_objects_pass_the_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        assert_eq!(loose_bytes(&dir).unwrap(), 0);
        grow(&dir.join("a.jsonl"), 1000, 0);
        commit(&dir, "ai", "one").unwrap();
        let loose = loose_bytes(&dir).unwrap();
        assert!(loose > 0);
        assert_eq!(gc(&dir, loose).unwrap(), Gc::Within);
        assert_eq!(gc(&dir, loose - 1).unwrap(), Gc::Started);
        packed(&dir);
    }

    /// `n` bytes zlib cannot shrink, differing by `seed`.
    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 32) as u8
            })
            .collect()
    }

    /// A repository whose gc has run and left loose objects of more than `limit` bytes that no
    /// commit holds, as a git without cruft packs leaves them: the folder to drop, the
    /// repository, and those bytes.
    fn with_leftovers(limit: u64) -> (tempfile::TempDir, PathBuf, u64) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        git(&dir, &["config", "gc.cruftPacks", "false"]);
        grow(&dir.join("a.jsonl"), 1000, 0);
        commit(&dir, "ai", "one").unwrap();
        let held = loose_bytes(&dir).unwrap();
        let stray = tmp.path().join("stray");
        fs::write(&stray, noise(2 * limit as usize, 1)).unwrap();
        git(&dir, &["hash-object", "-w", stray.to_str().unwrap()]);
        let left = loose_bytes(&dir).unwrap() - held;
        assert!(left > limit);

        assert_eq!(gc(&dir, limit).unwrap(), Gc::Started);
        settled(&dir, left);
        (tmp, dir, left)
    }

    /// Waits until a gc has left the repository at `dir` exactly `left` loose bytes and has ended,
    /// so its `gc.pid` turns no later gc away, up to a minute.
    fn settled(dir: &Path, left: u64) {
        for _ in 0..600 {
            if loose_bytes(dir).unwrap() == left && !git_dir(dir).join("gc.pid").exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the gc left {} loose bytes", loose_bytes(dir).unwrap());
    }

    /// Commits `n` new bytes zlib cannot shrink to the repository at `dir`, late enough that
    /// their objects are newer than any before them.
    fn commit_noise(dir: &Path, n: usize, seed: u64) {
        std::thread::sleep(Duration::from_millis(50));
        fs::write(dir.join(format!("n{seed}")), noise(n, seed)).unwrap();
        commit(dir, "ai", &format!("n{seed}")).unwrap();
    }

    #[test]
    fn loose_objects_a_gc_left_start_no_second_gc() {
        let limit = 50_000;
        let (_tmp, dir, left) = with_leftovers(limit);
        assert_eq!(gc(&dir, limit).unwrap(), Gc::Within);
        for seed in 2..5 {
            commit_noise(&dir, 1000, seed);
            assert!(loose_bytes(&dir).unwrap() > left);
            assert_eq!(gc(&dir, limit).unwrap(), Gc::Within);
        }
    }

    #[test]
    fn loose_objects_written_since_the_last_gc_start_the_next_past_the_limit() {
        let limit = 50_000;
        let (_tmp, dir, left) = with_leftovers(limit);
        commit_noise(&dir, limit as usize / 2, 2);
        assert_eq!(gc(&dir, limit).unwrap(), Gc::Within);
        commit_noise(&dir, limit as usize, 3);
        assert_eq!(gc(&dir, limit).unwrap(), Gc::Started);
        settled(&dir, left);
        git(&dir, &["fsck", "--strict", "--no-dangling"]);
        assert_eq!(gc(&dir, limit).unwrap(), Gc::Within);
    }

    #[test]
    fn without_a_usable_record_of_the_last_gc_every_loose_object_counts() {
        let limit = 50_000;
        let (_tmp, dir, left) = with_leftovers(limit);
        let mark = git_dir(&dir).join(GC_MARK);
        let recorded = fs::read_to_string(&mark).unwrap();
        assert_eq!(gc(&dir, limit).unwrap(), Gc::Within);

        let later = SystemTime::now() + Duration::from_secs(3600);
        let later = later.duration_since(UNIX_EPOCH).unwrap().as_nanos();
        for bad in ["", "soon\n", &format!("{later}\n")] {
            fs::write(&mark, bad).unwrap();
            assert_eq!(gc_mark(&dir).unwrap(), None, "{bad:?}");
        }
        fs::write(&mark, &recorded).unwrap();
        assert!(gc_mark(&dir).unwrap().is_some());

        commit_noise(&dir, 1000, 2);
        assert_eq!(gc(&dir, limit).unwrap(), Gc::Within);
        fs::remove_file(&mark).unwrap();
        assert_eq!(gc(&dir, limit).unwrap(), Gc::Started);
        assert_ne!(fs::read_to_string(&mark).unwrap(), recorded);
        settled(&dir, left);
    }

    #[test]
    fn commits_made_while_gc_runs_keep_every_object() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        let file = dir.join("t/items.jsonl");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let t = Path::new("t");
        for i in 0..40 {
            grow(&file, 20_000, i);
            commit_paths(&dir, &[t], "ai", &format!("c{i}")).unwrap();
        }
        let started = Command::new("git")
            .arg("--git-dir")
            .arg(git_dir(&dir))
            .args(["gc", "--quiet"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .spawn()
            .unwrap();
        let mut gc = started;
        let mut i = 40;
        loop {
            grow(&file, 20_000, i);
            commit_paths(&dir, &[t], "ai", &format!("c{i}")).unwrap();
            i += 1;
            if gc.try_wait().unwrap().is_some() && i >= 50 {
                break;
            }
        }
        assert!(gc.wait().unwrap().success());
        git(&dir, &["fsck", "--strict", "--no-dangling"]);
        assert_eq!(
            git(&dir, &["rev-list", "--count", "HEAD"]).trim(),
            i.to_string()
        );
        let (msg, text) = head(&dir, "t/items.jsonl");
        assert_eq!(msg, format!("c{}", i - 1));
        assert_eq!(text, fs::read_to_string(&file).unwrap());
    }

    #[test]
    fn a_commit_of_paths_that_cannot_move_head_leaves_the_index_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        let t = Path::new("t");
        fs::create_dir_all(dir.join(t)).unwrap();
        fs::write(dir.join("t/a"), "1\n").unwrap();
        commit_paths(&dir, &[t], "ai", "one").unwrap();

        let held = branch_lock(&dir);
        fs::write(&held, "").unwrap();
        fs::write(dir.join("t/b"), "2\n").unwrap();
        assert!(commit_paths(&dir, &[t], "ai", "two").is_err());
        assert_eq!(head(&dir, "t/a").0, "one");
        assert_eq!(staged(&dir), ["t/a"]);
        assert!(!git_dir(&dir).join("index.lock").exists());

        fs::remove_file(&held).unwrap();
        commit_paths(&dir, &[t], "ai", "two").unwrap();
        assert_eq!(head(&dir, "t/b"), ("two".into(), "2\n".into()));
        assert_eq!(staged(&dir), ["t/a", "t/b"]);
    }

    #[test]
    fn git_holding_the_index_lock_fails_a_commit_which_leaves_the_lock_alone() {
        let (_tmp, dir) = first_commit();
        let held = git_dir(&dir).join("index.lock");
        fs::write(&held, "git's").unwrap();
        let status = || {
            let work = dir.to_str().unwrap();
            git(&dir, &["--work-tree", work, "status", "--porcelain"])
        };
        commit(&dir, "ai", "nothing").unwrap();
        commit_paths(&dir, &[Path::new("first")], "ai", "nothing").unwrap();
        assert_eq!(subjects(&dir), "first\n", "nothing to commit needs no lock");

        fs::write(dir.join("a"), "1\n").unwrap();
        let e = error::line(&commit(&dir, "ai", "one").unwrap_err());
        let want = format!(
            "git's {} is held: a git command is using {}'s index. Let it finish; if none is \
             running, one that was cut short left the lock behind, and it can go.",
            pretty(&held),
            pretty(&dir)
        );
        assert_eq!(e, want);
        put(&dir, "t/b", "2\n");
        assert!(commit_paths(&dir, &[Path::new("t")], "ai", "two").is_err());
        assert_eq!(fs::read_to_string(&held).unwrap(), "git's");
        assert_eq!(subjects(&dir), "first\n");
        assert_eq!(staged(&dir), ["first"]);

        fs::remove_file(&held).unwrap();
        commit(&dir, "ai", "one").unwrap();
        assert_eq!(head(&dir, "a"), ("one".into(), "1\n".into()));
        assert_eq!(staged(&dir), ["a", "first", "t/b"]);
        assert_eq!(status(), "");
    }

    #[test]
    fn a_commit_waits_for_the_index_lock_while_git_holds_it() {
        let (_tmp, dir) = first_commit();
        git(&dir, &["config", "core.filesRefLockTimeout", "60000"]);
        let held = git_dir(&dir).join("index.lock");
        fs::write(&held, "git's").unwrap();
        fs::write(dir.join("a"), "1\n").unwrap();
        let committer = {
            let dir = dir.clone();
            std::thread::spawn(move || commit(&dir, "ai", "one"))
        };
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(subjects(&dir), "first\n");
        assert_eq!(fs::read_to_string(&held).unwrap(), "git's");
        fs::remove_file(&held).unwrap();
        committer.join().unwrap().unwrap();
        assert_eq!(head(&dir, "a"), ("one".into(), "1\n".into()));
        assert_eq!(staged(&dir), ["a", "first"]);
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

    #[test]
    fn head_holding_names_the_commit_only_while_its_tree_holds_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        let t = Path::new("t");
        assert_eq!(head_holding(&dir, t).unwrap(), None);
        fs::create_dir_all(dir.join("t")).unwrap();
        fs::write(dir.join("t/a"), "1\n").unwrap();
        commit_paths(&dir, &[t], "ai", "one").unwrap();
        let one = gix::open(&dir).unwrap().head_id().unwrap().detach();
        assert_eq!(head_holding(&dir, t).unwrap(), Some(one));
        assert_eq!(head_holding(&dir, Path::new("u")).unwrap(), None);
        fs::remove_dir_all(dir.join("t")).unwrap();
        commit_paths(&dir, &[t], "ai", "two").unwrap();
        assert_eq!(head_holding(&dir, t).unwrap(), None);
    }

    #[test]
    fn left_out_names_what_ignore_files_drop_and_what_is_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        fs::write(dir.join(".gitignore"), "*.tmp\n").unwrap();
        let t = dir.join("t");
        fs::create_dir_all(t.join("sub")).unwrap();
        fs::write(t.join("a"), "").unwrap();
        fs::write(t.join(".hidden"), "").unwrap();
        assert!(left_out(&dir, Path::new("t")).unwrap().is_empty());
        fs::write(t.join("sub/b.tmp"), "").unwrap();
        std::os::unix::fs::symlink(t.join("a"), t.join("link")).unwrap();
        assert_eq!(
            left_out(&dir, Path::new("t")).unwrap(),
            [t.join("link"), t.join("sub/b.tmp")]
        );
        assert!(left_out(&dir, Path::new("none")).unwrap().is_empty());
    }

    /// A repository at `store` in a new temporary directory, with a first commit.
    fn first_commit() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        fs::write(dir.join("first"), "").unwrap();
        commit(&dir, "ai", "first").unwrap();
        (tmp, dir)
    }

    /// Writes `text` to `rel` in `dir`, making its directory.
    fn put(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn subjects(dir: &Path) -> String {
        git(dir, &["log", "--format=%s"])
    }

    #[test]
    fn the_asks_waiting_are_committed_as_one_commit_in_the_order_asked() {
        let (_tmp, dir) = first_commit();
        assert_eq!(commit_asked(&dir, "ai").unwrap(), 0, "nothing waits");
        put(&dir, "a/x", "1\n");
        put(&dir, "b/y", "2\n");
        ask(&dir, Path::new("a"), "one").unwrap();
        ask(&dir, Path::new("b/"), "two").unwrap();
        ask(&dir, Path::new("a"), "one").unwrap();
        ask(&dir, Path::new("a"), "three").unwrap();
        assert_eq!(commit_asked(&dir, "ai").unwrap(), 4);
        assert_eq!(subjects(&dir), "one; two; three\nfirst\n");
        assert_eq!(head(&dir, "b/y").1, "2\n");
        assert!(asked(&dir).unwrap().is_empty());
        assert_eq!(commit_asked(&dir, "ai").unwrap(), 0);
        assert_eq!(subjects(&dir), "one; two; three\nfirst\n");
    }

    #[test]
    fn an_ask_made_while_a_commit_waits_on_git_is_committed_by_that_committer() {
        let (_tmp, dir) = first_commit();
        let mut config = fs::OpenOptions::new()
            .append(true)
            .open(git_dir(&dir).join("config"))
            .unwrap();
        writeln!(config, "[core]\n\tfilesRefLockTimeout = 60000").unwrap();
        let branch = gix::open(&dir).unwrap().head_name().unwrap().unwrap();
        let held = git_dir(&dir).join(format!("{}.lock", branch.as_bstr()));
        fs::write(&held, "").unwrap();
        put(&dir, "a/x", "1\n");
        ask(&dir, Path::new("a"), "one").unwrap();
        let committer = {
            let dir = dir.clone();
            std::thread::spawn(move || commit_asked(&dir, "ai"))
        };
        let started = std::time::Instant::now();
        while !git_dir(&dir).join("HEAD.lock").exists() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "no commit began"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(committing(&dir).unwrap());
        put(&dir, "b/y", "2\n");
        ask(&dir, Path::new("b"), "two").unwrap();
        fs::remove_file(&held).unwrap();
        assert_eq!(committer.join().unwrap().unwrap(), 2);
        assert_eq!(subjects(&dir), "two\none\nfirst\n");
        assert_eq!(head(&dir, "b/y").1, "2\n");
        assert!(!committing(&dir).unwrap());
    }

    #[test]
    fn a_failed_commit_is_recorded_and_keeps_its_asks_until_one_succeeds() {
        let (_tmp, dir) = first_commit();
        let branch = gix::open(&dir).unwrap().head_name().unwrap().unwrap();
        let stale = git_dir(&dir).join(format!("{}.lock", branch.as_bstr()));
        fs::write(&stale, "").unwrap();
        put(&dir, "a/x", "1\n");
        ask(&dir, Path::new("a"), "one").unwrap();
        let e = error::line(&commit_asked(&dir, "ai").unwrap_err());
        assert!(e.starts_with("Cannot commit "), "{e}");
        assert!(e.contains(".lock"), "{e}");
        assert_eq!(failed(&dir).unwrap().as_deref(), Some(e.as_str()));
        put(&dir, "b/y", "2\n");
        ask(&dir, Path::new("b"), "two").unwrap();
        assert!(commit_asked(&dir, "ai").is_err());
        assert_eq!(asked(&dir).unwrap().len(), 2, "what failed stays queued");

        fs::remove_file(&stale).unwrap();
        assert_eq!(commit_asked(&dir, "ai").unwrap(), 2);
        assert_eq!(failed(&dir).unwrap(), None);
        assert_eq!(subjects(&dir), "one; two\nfirst\n");
    }
}
