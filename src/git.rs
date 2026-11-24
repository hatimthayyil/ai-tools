use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use gix::{
    ObjectId, actor::SignatureRef, lock::acquire::Fail, objs::tree::EntryKind,
    refs::store::WriteReflog,
};

use crate::file::{AtPath, pretty};

/// The blob at a store's tree root naming its format.
pub const FORMAT: &str = "format";

/// The sha1 blob id of `input`: the id git gives a file holding it.
pub fn hash(input: &str) -> Result<String> {
    let id = gix::objs::compute_hash(
        gix::hash::Kind::Sha1,
        gix::objs::Kind::Blob,
        input.as_bytes(),
    )?;
    Ok(id.to_string())
}

/// Entry `n` under `dir`, fanned out 256 per tree.
pub fn fan_path(dir: &str, n: u64) -> String {
    format!("{dir}/{:04x}/{:02x}", n >> 8, n & 255)
}

/// New blobs by path, and paths removed with all under them; removals apply first.
#[derive(Debug, Default)]
pub struct Changes {
    put: BTreeMap<String, Vec<u8>>,
    gone: BTreeSet<String>,
}

impl Changes {
    pub fn put(&mut self, path: String, bytes: Vec<u8>) {
        self.put.insert(path, bytes);
    }

    pub fn remove(&mut self, path: String) {
        self.gone.insert(path);
    }

    pub fn is_empty(&self) -> bool {
        self.put.is_empty() && self.gone.is_empty()
    }
}

/// How long a lock is waited for where `core.filesRefLockTimeout` gives no time: git's own wait
/// for a ref's.
const LOCK_WAIT: Duration = Duration::from_millis(100);
/// How often taking a ref's lock starts over when a directory above it goes.
const RETAKES: usize = 32;
/// The refs git makes a reflog for where `core.logAllRefUpdates` is `true`, as git's
/// `should_autocreate_reflog` names them, besides HEAD.
const LOGGED: [&str; 3] = ["refs/heads/", "refs/remotes/", "refs/notes/"];

/// The file the ref `name`, one under `refs/`, has to itself in `repo` until git packs it.
fn loose(repo: &gix::Repository, name: &str) -> PathBuf {
    repo.refs.common_dir_resolved().join(name)
}

/// The commit the ref `name`, one under `refs/`, names in `repo`; `None` without the ref.
///
/// It is read as git reads it: the ref's own file, and only without one a `packed-refs` opened
/// for this read. gix looks in the `packed-refs` it last read, then at the ref's file, so a ref
/// git packs between the two reads as it was before, or as absent.
pub fn tip(repo: &gix::Repository, name: &str) -> Result<Option<ObjectId>> {
    let path = loose(repo, name);
    match fs::read(&path) {
        Ok(text) => match ObjectId::from_hex(text.trim_ascii_end()) {
            Ok(id) => Ok(Some(id)),
            Err(_) => bail!("{} names no commit.", pretty(&path)),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound || path.is_dir() => {
            let Some(packed) = repo.refs.open_packed_buffer().map_err(gix::Error::from)? else {
                return Ok(None);
            };
            let found = packed.try_find(name).map_err(gix::Error::from)?;
            Ok(found.map(|r| r.target()))
        }
        Err(e) => Err(e).at(&path),
    }
}

/// How long `repo`'s git waits for a ref's lock: `core.filesRefLockTimeout` milliseconds, for
/// ever when negative.
fn wait(repo: &gix::Repository) -> Fail {
    match repo.config_snapshot().integer("core.filesRefLockTimeout") {
        None => LOCK_WAIT.into(),
        Some(ms) if ms < 0 => Fail::AfterDurationWithBackoff(Duration::from_secs(u64::MAX)),
        Some(ms) => Duration::from_millis(ms.unsigned_abs()).into(),
    }
}

/// The lock git takes to rewrite the file at `path`: `<path>.lock`, made only where there is
/// none, with mode 0666 less the umask as git makes it, and with the directories up to
/// `boundary`; waited for as long as `repo`'s git waits for a ref's lock. Dropped, it goes,
/// and so does every directory it made; a signal that ends `ai` takes it too.
pub fn lock(
    repo: &gix::Repository,
    path: &Path,
    boundary: Option<PathBuf>,
) -> gix::ExnResult<gix::lock::File> {
    let wait = wait(repo);
    retaking(|| {
        gix::lock::File::acquire_to_update_resource_with_permissions(
            path,
            wait,
            boundary.clone(),
            || fs::Permissions::from_mode(0o666),
        )
    })
}

/// The lock of the ref `name`, one under `refs/`: `<ref>.lock` beside the ref's file.
fn ref_lock(repo: &gix::Repository, name: &str) -> gix::ExnResult<gix::lock::File> {
    let refs = repo.refs.common_dir_resolved().join("refs");
    lock(repo, &loose(repo, name), Some(refs))
}

/// The lock of the ref `name`, one under `refs/`, while it names `from`; `None` once it names
/// another commit. The ref is read under the lock, which every writer of the ref takes, and git
/// too before it removes the file of a ref it packed: the value read there stands until the
/// lock goes.
fn ref_lock_at(
    repo: &gix::Repository,
    name: &str,
    from: Option<ObjectId>,
) -> Result<Option<gix::lock::File>> {
    let lock = match ref_lock(repo, name) {
        Ok(lock) => lock,
        Err(_) if tip(repo, name)? != from => return Ok(None),
        Err(e) => return Err(gix::Error::from(e).into()),
    };
    Ok((tip(repo, name)? == from).then_some(lock))
}

/// Writes `to` into `lock`, a ref's or a detached HEAD's, and renames it over the file it locks.
fn put(mut lock: gix::lock::File, to: ObjectId) -> Result<()> {
    lock.with_mut(|f| writeln!(f, "{to}"))
        .at(lock.lock_path())?;
    let path = lock.resource_path();
    match lock.commit() {
        Ok(_) => Ok(()),
        // Empty directories where the ref's file belongs: what is left of refs once under its name.
        Err(e) if path.is_dir() => {
            gix::tempfile::remove_dir::empty_depth_first(path.clone()).at(&path)?;
            let moved = e.instance.commit().map(|_| ());
            moved.map_err(|e| e.error).at(&path)
        }
        Err(e) => Err(e.error).at(&path),
    }
}

/// Moves the ref `name` of `repo`, one under `refs/`, from `from` to `to`, as git moves a ref;
/// false if another writer moved it first. It writes no reflog.
pub fn swap(
    repo: &gix::Repository,
    name: &str,
    from: Option<ObjectId>,
    to: ObjectId,
) -> Result<bool> {
    let Some(lock) = ref_lock_at(repo, name, from)? else {
        return Ok(false);
    };
    put(lock, to)?;
    Ok(true)
}

/// HEAD of a repository as it was read.
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    /// The branch HEAD names, a ref under `refs/`; `None` for a detached HEAD.
    pub branch: Option<String>,
    /// HEAD's commit; `None` on a branch with no commit yet.
    pub commit: Option<ObjectId>,
}

/// HEAD of `repo` as git reads it: its file, then the branch it names as [`tip`] reads it.
pub fn head(repo: &gix::Repository) -> Result<Head> {
    let path = repo.git_dir().join("HEAD");
    let text = fs::read(&path).at(&path)?;
    let text = text.trim_ascii_end();
    let Some(name) = text.strip_prefix(b"ref: ") else {
        return match ObjectId::from_hex(text) {
            Ok(id) => Ok(Head {
                branch: None,
                commit: Some(id),
            }),
            Err(_) => bail!("{} names no commit.", pretty(&path)),
        };
    };
    let Some(name) = std::str::from_utf8(name)
        .ok()
        .filter(|n| n.starts_with("refs/"))
    else {
        bail!("{} names no branch.", pretty(&path));
    };
    Ok(Head {
        commit: tip(repo, name)?,
        branch: Some(name.into()),
    })
}

/// Moves HEAD of `repo` from `from` to `to`, a commit whose subject begins `msg` and whose
/// committer is `who`, as `git commit` moves it; false if HEAD names another commit by then.
///
/// git takes `HEAD.lock`, then the lock of the branch HEAD names, and reads the branch under
/// them; it appends the move to HEAD's reflog and the branch's, renames the branch's lock over
/// its file, and lets `HEAD.lock` go, leaving HEAD naming the branch. A detached HEAD is itself
/// the ref written.
pub fn advance_head(
    repo: &gix::Repository,
    from: Option<ObjectId>,
    to: ObjectId,
    who: SignatureRef<'_>,
    msg: &str,
) -> Result<bool> {
    let path = repo.git_dir().join("HEAD");
    let held = match lock(repo, &path, None) {
        Ok(lock) => lock,
        Err(_) if head(repo)?.commit != from => return Ok(false),
        Err(e) => return Err(gix::Error::from(e).into()),
    };
    let subject = msg.lines().next().unwrap_or_default().trim_end();
    let why = match from {
        None => format!("commit (initial): {subject}"),
        Some(_) => format!("commit: {subject}"),
    };
    let entry = Entry {
        from,
        to,
        who,
        why: &why,
    };
    match head(repo)?.branch {
        Some(branch) => {
            let Some(lock) = ref_lock_at(repo, &branch, from)? else {
                return Ok(false);
            };
            log(repo, "HEAD", &entry)?;
            log(repo, &branch, &entry)?;
            put(lock, to)?;
            drop(held);
        }
        None => {
            if head(repo)?.commit != from {
                return Ok(false);
            }
            log(repo, "HEAD", &entry)?;
            put(held, to)?;
        }
    }
    Ok(true)
}

/// A move of a ref, as its reflog records it.
struct Entry<'a> {
    from: Option<ObjectId>,
    to: ObjectId,
    who: SignatureRef<'a>,
    why: &'a str,
}

/// Appends `e` to the reflog of `name` in `repo`, HEAD or a ref under `refs/`, as git does: to
/// the reflog there is, else to a new one where `core.logAllRefUpdates` has git make one.
fn log(repo: &gix::Repository, name: &str, e: &Entry) -> Result<()> {
    let base = match name {
        "HEAD" => repo.git_dir(),
        _ => repo.refs.common_dir_resolved(),
    };
    let path = base.join("logs").join(name);
    let make = match repo.refs.write_reflog {
        WriteReflog::Always => true,
        WriteReflog::Normal => name == "HEAD" || LOGGED.iter().any(|p| name.starts_with(p)),
        WriteReflog::Disable => false,
    };
    let open = || fs::OpenOptions::new().append(true).create(make).open(&path);
    if make {
        let dir = path.parent().expect("a reflog is under logs/");
        fs::create_dir_all(dir).at(dir)?;
    }
    let mut f = match open() {
        Ok(f) => f,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        // Empty directories where the reflog belongs: what is left of refs once under its name.
        Err(_) if make && path.is_dir() => {
            gix::tempfile::remove_dir::empty_depth_first(path.clone()).at(&path)?;
            open().at(&path)?
        }
        Err(err) => return Err(err).at(&path),
    };
    let mut line = Vec::new();
    let none = repo.object_hash().null();
    write!(line, "{} {} ", e.from.unwrap_or(none), e.to)?;
    e.who.trim().write_to(&mut line)?;
    writeln!(line, "\t{}", e.why)?;
    f.write_all(&line).at(&path)
}

/// What `take` gives, taking again while it fails for a missing directory.
///
/// Taking a ref's lock makes the directories above it, and a lock given up removes those it
/// leaves empty, as a packed ref's are: one can go between its making and the lock's.
fn retaking<T>(mut take: impl FnMut() -> gix::ExnResult<T>) -> gix::ExnResult<T> {
    let mut left = RETAKES;
    loop {
        match take() {
            Err(e)
                if left > 0
                    && e.downcast_any_ref::<io::Error>()
                        .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                left -= 1;
            }
            taken => return taken,
        }
    }
}

/// A store under one git ref, one under `refs/`: a tree of blobs, one commit per mutation,
/// compare-and-swap on the ref. `what` names the store in messages; `format` is the format this
/// ai writes.
pub struct Ref {
    repo: gix::Repository,
    name: String,
    what: &'static str,
    format: u32,
}

impl Ref {
    pub fn new(repo: gix::Repository, name: String, what: &'static str, format: u32) -> Ref {
        Ref {
            repo,
            name,
            what,
            format,
        }
    }

    pub fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The commit the ref names; `None` without the ref.
    pub fn head(&self) -> Result<Option<ObjectId>> {
        tip(&self.repo, &self.name)
    }

    /// The tree of `commit`, of any format; `None` is the empty store.
    pub fn raw(&self, commit: Option<ObjectId>) -> Result<Snap<'_>> {
        let tree = match commit {
            Some(c) => self.repo.find_commit(c)?.tree_id()?.detach(),
            None => ObjectId::empty_tree(self.repo.object_hash()),
        };
        Ok(self.tree(commit, tree))
    }

    /// The tree `tree`, read as if `commit` held it.
    pub fn tree(&self, commit: Option<ObjectId>, tree: ObjectId) -> Snap<'_> {
        Snap {
            git: self,
            commit,
            tree,
            cache: RefCell::default(),
        }
    }

    /// The format a `format` blob names; none is format 1.
    pub fn parse_format(&self, b: &[u8]) -> Result<u32> {
        if b.is_empty() {
            return Ok(1);
        }
        match std::str::from_utf8(b)
            .ok()
            .and_then(|s| s.trim_end().parse().ok())
        {
            Some(v) if v > 0 => Ok(v),
            _ => bail!("This {}'s format blob is corrupt.", self.what),
        }
    }

    /// Refuses a format newer than this ai's.
    pub fn readable(&self, v: u32) -> Result<()> {
        if v > self.format {
            bail!(
                "This {} is format {v}; this ai reads up to {}. Upgrade ai.",
                self.what,
                self.format
            );
        }
        Ok(())
    }

    /// Applies `change` to the store `at` gives for the head as one commit, redoing it until
    /// the ref update wins; returns its result and the store after it.
    pub fn mutate<'r, S: AsRef<Snap<'r>>, R>(
        &'r self,
        msg: &str,
        at: impl Fn(Option<ObjectId>) -> Result<S>,
        mut change: impl FnMut(&S) -> Result<(R, Changes)>,
    ) -> Result<(R, S)> {
        loop {
            let base = at(self.head()?)?;
            let (r, ch) = change(&base)?;
            if ch.is_empty() {
                return Ok((r, base));
            }
            let b = base.as_ref();
            let commit = self.commit(b, ch, msg)?;
            if self.cas(b.commit, commit)? {
                return Ok((r, at(Some(commit))?));
            }
        }
    }

    /// `ch` over `base`, the first commit naming the format.
    fn commit(&self, base: &Snap, mut ch: Changes, msg: &str) -> Result<ObjectId> {
        if base.commit.is_none() {
            ch.put(FORMAT.into(), format_blob(self.format));
        }
        let tree = self.write_tree(base.tree, ch)?;
        self.new_commit(msg, tree, base.commit)
    }

    pub fn write_tree(&self, base: ObjectId, Changes { put, gone }: Changes) -> Result<ObjectId> {
        let mut ed = self.repo.edit_tree(base)?;
        for p in gone {
            ed.remove(&p)?;
        }
        for (p, bytes) in put {
            let id = self.repo.write_blob(bytes)?;
            ed.upsert(&p, EntryKind::Blob, id)?;
        }
        Ok(ed.write()?.detach())
    }

    pub fn new_commit(
        &self,
        msg: &str,
        tree: ObjectId,
        parent: Option<ObjectId>,
    ) -> Result<ObjectId> {
        let me = self.repo.committer().context("No committer.")??;
        Ok(self.repo.new_commit_as(me, me, msg, tree, parent)?.id)
    }

    /// Moves the ref from `from` to `to` ([`swap`]); false if another writer moved it first.
    pub fn cas(&self, from: Option<ObjectId>, to: ObjectId) -> Result<bool> {
        swap(&self.repo, &self.name, from, to)
    }
}

/// The `format` blob of format `v`.
pub fn format_blob(v: u32) -> Vec<u8> {
    format!("{v}\n").into_bytes()
}

/// A store as of one commit: every read through it sees the same state.
pub struct Snap<'r> {
    git: &'r Ref,
    commit: Option<ObjectId>,
    tree: ObjectId,
    cache: RefCell<HashMap<String, Rc<[u8]>>>,
}

impl<'r> AsRef<Snap<'r>> for Snap<'r> {
    fn as_ref(&self) -> &Snap<'r> {
        self
    }
}

impl Snap<'_> {
    pub fn tree_id(&self) -> ObjectId {
        self.tree
    }

    /// The format its `format` blob names; the empty store is of this ai's format.
    pub fn format(&self) -> Result<u32> {
        match self.commit {
            Some(_) => self.git.parse_format(&self.read(FORMAT)?),
            None => Ok(self.git.format),
        }
    }

    pub fn entry(&self, path: &str) -> Result<Option<ObjectId>> {
        let root = self.git.repo.find_tree(self.tree)?;
        Ok(root.lookup_entry_by_path(path)?.map(|e| e.object_id()))
    }

    pub fn fetch(&self, id: ObjectId) -> Result<Vec<u8>> {
        Ok(self.git.repo.find_blob(id)?.take_data())
    }

    /// The blob at `path`, empty if absent; cached.
    pub fn read(&self, path: &str) -> Result<Rc<[u8]>> {
        if let Some(b) = self.cache.borrow().get(path) {
            return Ok(b.clone());
        }
        let b: Rc<[u8]> = match self.entry(path)? {
            Some(id) => self.fetch(id)?.into(),
            None => Rc::from([]),
        };
        self.cache.borrow_mut().insert(path.into(), b.clone());
        Ok(b)
    }

    fn named(&self, tree: ObjectId) -> Result<Vec<(String, ObjectId)>> {
        let t = self.git.repo.find_tree(tree)?;
        Ok(t.decode()?
            .entries
            .iter()
            .map(|e| (e.filename.to_string(), e.oid.to_owned()))
            .collect())
    }

    /// A tree's entries by their hex names, in order.
    fn numbered(&self, tree: ObjectId) -> Result<Vec<(u64, ObjectId)>> {
        self.named(tree)?
            .into_iter()
            .map(|(name, id)| {
                let n = u64::from_str_radix(&name, 16)
                    .with_context(|| format!("An entry is misnamed: {name}."))?;
                Ok((n, id))
            })
            .collect()
    }

    /// Every blob under the fanned-out `dir`, with its number.
    pub fn walk(&self, dir: &str, mut each: impl FnMut(u64, ObjectId) -> Result<()>) -> Result<()> {
        let Some(top) = self.entry(dir)? else {
            return Ok(());
        };
        for (hi, sub) in self.numbered(top)? {
            for (lo, id) in self.numbered(sub)? {
                each(hi << 8 | lo, id)?;
            }
        }
        Ok(())
    }

    /// One past the highest number under the fanned-out `dir`; 0 if empty.
    pub fn count(&self, dir: &str) -> Result<u64> {
        let Some(top) = self.entry(dir)? else {
            return Ok(0);
        };
        let Some(&(hi, sub)) = self.numbered(top)?.last() else {
            return Ok(0);
        };
        let Some(&(lo, _)) = self.numbered(sub)?.last() else {
            return Ok(0);
        };
        Ok((hi << 8 | lo) + 1)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        thread,
        time::Instant,
    };

    use super::*;
    use crate::{error, repo};

    const NAME: &str = "refs/ai/test";

    /// The ref [`NAME`] of the bare repository at `dir`, which is made if there is none.
    fn store(dir: &Path) -> Ref {
        if !dir.join("HEAD").exists() {
            gix::init_bare(dir).unwrap();
        }
        let mut repo = gix::open(dir).unwrap();
        repo.committer_or_set_fallback("ai", "ai@localhost")
            .unwrap();
        Ref::new(repo, NAME.into(), "test", 1)
    }

    /// A new commit on `parent`; `msg` tells it from every other on that parent.
    fn commit(r: &Ref, parent: Option<ObjectId>, msg: &str) -> ObjectId {
        let mut ch = Changes::default();
        ch.put("note".into(), msg.as_bytes().to_vec());
        let tree = r
            .write_tree(ObjectId::empty_tree(r.repo().object_hash()), ch)
            .unwrap();
        r.new_commit(msg, tree, parent).unwrap()
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = repo::git(dir).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Packs every ref of the repository at `dir` and removes the packed refs' own files.
    fn pack(dir: &Path) {
        git(dir, &["pack-refs", "--all", "--prune"]);
    }

    fn packed_refs(dir: &Path) -> String {
        fs::read_to_string(dir.join("packed-refs")).unwrap_or_default()
    }

    #[test]
    fn a_ref_is_made_only_where_there_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let r = store(tmp.path());
        assert_eq!(r.head().unwrap(), None);
        let (a, b) = (commit(&r, None, "a"), commit(&r, None, "b"));
        assert!(!r.cas(Some(a), b).unwrap(), "no ref to move from a");
        assert_eq!(r.head().unwrap(), None);
        assert!(r.cas(None, a).unwrap());
        assert_eq!(r.head().unwrap(), Some(a));
        assert!(!r.cas(None, b).unwrap(), "the ref is there now");
        assert_eq!(r.head().unwrap(), Some(a));
    }

    #[test]
    fn a_ref_in_its_own_file_moves_from_the_commit_it_names_and_no_other() {
        let tmp = tempfile::tempdir().unwrap();
        let r = store(tmp.path());
        let a = commit(&r, None, "a");
        let (b, c) = (commit(&r, Some(a), "b"), commit(&r, Some(a), "c"));
        assert!(r.cas(None, a).unwrap());
        let file = tmp.path().join(NAME);
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("{a}\n"));
        assert!(r.cas(Some(a), b).unwrap());
        assert!(!r.cas(Some(a), c).unwrap(), "another writer moved it first");
        assert_eq!(r.head().unwrap(), Some(b));
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("{b}\n"));
        assert!(!tmp.path().join(format!("{NAME}.lock")).exists());
        assert_eq!(packed_refs(tmp.path()), "", "no write packs a ref");
        assert!(!tmp.path().join("logs").join(NAME).exists(), "no reflog");
        git(tmp.path(), &["fsck", "--strict"]);
    }

    #[test]
    fn a_packed_ref_is_read_and_moved_from_packed_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let r = store(tmp.path());
        let a = commit(&r, None, "a");
        let (b, c) = (commit(&r, Some(a), "b"), commit(&r, Some(a), "c"));
        assert!(r.cas(None, a).unwrap());
        pack(tmp.path());
        let file = tmp.path().join(NAME);
        assert!(!file.exists(), "git removed the packed ref's file");
        assert!(packed_refs(tmp.path()).contains(&format!("{a} {NAME}")));
        assert_eq!(r.head().unwrap(), Some(a));
        assert!(!r.cas(None, b).unwrap(), "the ref is there, packed");
        assert!(!r.cas(Some(c), b).unwrap());
        assert!(
            tmp.path().join("refs").is_dir(),
            "a lock given up leaves refs/, without which this is no repository"
        );
        assert!(r.cas(Some(a), b).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("{b}\n"));
        assert_eq!(r.head().unwrap(), Some(b));
    }

    #[test]
    fn a_ref_s_own_file_wins_over_packed_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let r = store(tmp.path());
        let a = commit(&r, None, "a");
        let b = commit(&r, Some(a), "b");
        let c = commit(&r, Some(b), "c");
        assert!(r.cas(None, a).unwrap());
        pack(tmp.path());
        assert!(r.cas(Some(a), b).unwrap());
        assert!(packed_refs(tmp.path()).contains(&format!("{a} {NAME}")));
        assert_eq!(r.head().unwrap(), Some(b));
        assert!(
            !r.cas(Some(a), c).unwrap(),
            "a is the packed value of before"
        );
        assert!(r.cas(Some(b), c).unwrap());
        assert_eq!(r.head().unwrap(), Some(c));
    }

    #[test]
    fn a_ref_git_packed_since_this_repository_was_opened_is_read_as_it_is_now() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, other) = (store(tmp.path()), store(tmp.path()));
        let a = commit(&r, None, "a");
        assert!(r.cas(None, a).unwrap());
        pack(tmp.path());
        assert_eq!(r.head().unwrap(), Some(a));
        let b = commit(&other, Some(a), "b");
        assert!(other.cas(Some(a), b).unwrap());
        pack(tmp.path());
        assert_eq!(r.head().unwrap(), Some(b));
        let c = commit(&r, Some(a), "c");
        assert!(
            !r.cas(Some(a), c).unwrap(),
            "a is what packed-refs held before"
        );
        assert_eq!(other.head().unwrap(), Some(b));
    }

    #[test]
    fn empty_directories_at_the_ref_s_name_are_no_ref_and_give_way_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let r = store(tmp.path());
        let file = tmp.path().join(NAME);
        fs::create_dir_all(file.join("left/over")).unwrap();
        assert_eq!(r.head().unwrap(), None);
        let a = commit(&r, None, "a");
        assert!(r.cas(None, a).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("{a}\n"));
    }

    #[test]
    fn a_ref_file_naming_no_commit_is_an_error_naming_it() {
        let tmp = tempfile::tempdir().unwrap();
        let r = store(tmp.path());
        let a = commit(&r, None, "a");
        assert!(r.cas(None, a).unwrap());
        pack(tmp.path());
        let file = tmp.path().join(NAME);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        for text in ["", "ref: refs/heads/main\n", "0123\n"] {
            fs::write(&file, text).unwrap();
            let said = format!("{} names no commit.", pretty(&file));
            assert_eq!(r.head().unwrap_err().to_string(), said);
            let b = commit(&r, Some(a), "b");
            assert_eq!(r.cas(Some(a), b).unwrap_err().to_string(), said);
            assert_eq!(fs::read_to_string(&file).unwrap(), text);
        }
    }

    #[test]
    fn a_held_lock_fails_the_move_after_the_wait_naming_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let r = store(tmp.path());
        let a = commit(&r, None, "a");
        let b = commit(&r, Some(a), "b");
        assert!(r.cas(None, a).unwrap());
        let lock = tmp.path().join(format!("{NAME}.lock"));
        fs::write(&lock, "").unwrap();
        let began = Instant::now();
        let e = r.cas(Some(a), b).unwrap_err();
        assert!(began.elapsed() >= LOCK_WAIT, "{:?}", began.elapsed());
        let l = error::line(&e);
        assert_eq!(l.lines().count(), 1, "{l}");
        assert!(l.contains("could not be obtained after 0.10s"), "{l}");
        assert!(
            l.contains(&format!("The lockfile at '{}'", lock.display())),
            "{l}"
        );
        assert_eq!(r.head().unwrap(), Some(a));
        assert!(lock.exists(), "another's lock is left alone");
        assert!(
            !r.cas(Some(b), a).unwrap(),
            "a move from a commit the ref does not name needs no lock to fail"
        );
    }

    #[test]
    fn the_wait_for_a_held_lock_is_the_one_git_is_set_to() {
        let tmp = tempfile::tempdir().unwrap();
        let a = commit(&store(tmp.path()), None, "a");
        git(tmp.path(), &["config", "core.filesRefLockTimeout", "0"]);
        let r = store(tmp.path());
        fs::create_dir_all(tmp.path().join(NAME).parent().unwrap()).unwrap();
        fs::write(tmp.path().join(format!("{NAME}.lock")), "").unwrap();
        let l = error::line(&r.cas(None, a).unwrap_err());
        assert!(l.contains("could not be obtained immediately"), "{l}");
    }

    /// A lock that fails for a missing directory, as one fails whose directory went after it
    /// was made.
    fn in_no_directory(tmp: &Path) -> gix::ExnResult<gix::lock::File> {
        gix::lock::File::acquire_to_update_resource(tmp.join("gone/ref"), Fail::Immediately, None)
    }

    #[test]
    fn a_lock_whose_directory_went_is_taken_again() {
        let tmp = tempfile::tempdir().unwrap();
        let mut tries = 0;
        let lock = retaking(|| {
            tries += 1;
            match tries {
                1..=3 => in_no_directory(tmp.path()),
                _ => gix::lock::File::acquire_to_update_resource(
                    tmp.path().join("ref"),
                    Fail::Immediately,
                    None,
                ),
            }
        });
        assert_eq!(lock.unwrap().lock_path(), tmp.path().join("ref.lock"));
        assert_eq!(tries, 4);
    }

    #[test]
    fn a_lock_whose_directory_keeps_going_fails_for_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut tries = 0;
        let e = retaking(|| {
            tries += 1;
            in_no_directory(tmp.path())
        })
        .unwrap_err();
        assert_eq!(tries, RETAKES + 1);
        let l = error::line(&gix::Error::from(e).into());
        assert!(l.contains("No such file or directory"), "{l}");
    }

    #[test]
    fn a_held_lock_is_not_taken_again() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("ref.lock"), "").unwrap();
        let mut tries = 0;
        let taken = retaking(|| {
            tries += 1;
            gix::lock::File::acquire_to_update_resource(
                tmp.path().join("ref"),
                Fail::Immediately,
                None,
            )
        });
        assert!(taken.is_err());
        assert_eq!(tries, 1);
    }

    /// `writers` threads, each a writer of its own with the repository opened for itself, move
    /// one ref `notes` times each while git packs it without pause. Gives the commits writers
    /// were told they moved the ref to that its history lacks, and what failed.
    fn race(writers: usize, notes: usize) -> (usize, Vec<String>) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let first = store(dir);
        // Waits as long as a loaded machine needs, so that no writer fails for a held lock.
        git(dir, &["config", "core.filesRefLockTimeout", "60000"]);
        git(dir, &["config", "core.packedRefsTimeout", "60000"]);
        assert!(first.cas(None, commit(&first, None, "first")).unwrap());
        pack(dir);
        let (done, left) = (AtomicBool::new(false), AtomicUsize::new(writers));
        let (saved, failed) = thread::scope(|s| {
            let packer = s.spawn(|| {
                let mut failed = Vec::new();
                while !done.load(Ordering::Relaxed) {
                    let out = repo::git(dir)
                        .args(["pack-refs", "--all", "--prune"])
                        .output()
                        .unwrap();
                    if !out.status.success() {
                        failed.push(String::from_utf8_lossy(&out.stderr).trim().to_owned());
                    }
                }
                failed
            });
            let kids: Vec<_> = (0..writers)
                .map(|w| {
                    let (done, left) = (&done, &left);
                    s.spawn(move || {
                        let r = store(dir);
                        let (mut saved, mut failed) = (Vec::new(), Vec::new());
                        for n in 0..notes {
                            loop {
                                let head = r.head().unwrap();
                                let to = commit(&r, head, &format!("{w} {n}"));
                                match r.cas(head, to) {
                                    Ok(true) => break saved.push(to),
                                    Ok(false) => {}
                                    Err(e) => break failed.push(error::line(&e)),
                                }
                            }
                        }
                        if left.fetch_sub(1, Ordering::Relaxed) == 1 {
                            done.store(true, Ordering::Relaxed);
                        }
                        (saved, failed)
                    })
                })
                .collect();
            let (mut saved, mut failed) = (Vec::new(), Vec::new());
            for k in kids {
                let (s, f) = k.join().unwrap();
                saved.extend(s);
                failed.extend(f);
            }
            failed.extend(packer.join().unwrap());
            (saved, failed)
        });
        let end = store(dir);
        let head = end.head().unwrap().unwrap();
        let kept: std::collections::HashSet<ObjectId> = end
            .repo()
            .rev_walk([head])
            .all()
            .unwrap()
            .map(|c| c.unwrap().id)
            .collect();
        let lost = saved.iter().filter(|c| !kept.contains(*c)).count();
        (lost, failed)
    }

    /// Races `rounds` times: no commit may be lost, and no writer and no packing may fail.
    fn races(rounds: usize) {
        for round in 0..rounds {
            let (lost, failed) = race(8, 16);
            assert_eq!(lost, 0, "round {round}");
            assert_eq!(failed, Vec::<String>::new(), "round {round}");
        }
    }

    #[test]
    fn no_commit_is_lost_while_git_packs_the_ref_under_its_writers() {
        races(8);
    }

    /// Enough races to meet a loss that one race in twenty shows.
    #[test]
    #[ignore = "takes half a minute"]
    fn no_commit_is_lost_over_many_races_with_git_packing_the_ref() {
        races(100);
    }
}
