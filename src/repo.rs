use std::{
    collections::HashMap,
    fs, io,
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

use crate::file::{self, AtPath, pretty};

const GIT: &str = ".git";
const LOCK: &str = "ai-commit.lock";
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
    let mut c = Command::new("git");
    for k in GIT_ENV {
        c.env_remove(k);
    }
    let child = c
        .arg("--git-dir")
        .arg(git_dir(dir))
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
    // Before HEAD moves, as in `commit`.
    index.write(Default::default())?;
    if !same {
        let me = repo.committer().context("No committer.")??;
        repo.commit_as(me, me, "HEAD", msg, tree, parent)?;
    }
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
    fn a_commit_of_paths_writes_the_index_before_head_moves() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("store");
        create(&dir).unwrap();
        let t = Path::new("t");
        fs::create_dir_all(dir.join(t)).unwrap();
        fs::write(dir.join("t/a"), "1\n").unwrap();
        commit_paths(&dir, &[t], "ai", "one").unwrap();
        let staged = || {
            let index = gix::open(&dir).unwrap().index_or_empty().unwrap();
            let paths = index.entries().iter().map(|e| e.path(&index).to_string());
            paths.collect::<Vec<_>>()
        };

        // A held lock on HEAD's branch keeps HEAD where it is.
        let branch = gix::open(&dir).unwrap().head_name().unwrap().unwrap();
        let held = git_dir(&dir).join(format!("{}.lock", branch.as_bstr()));
        fs::write(&held, "").unwrap();
        fs::write(dir.join("t/b"), "2\n").unwrap();
        assert!(commit_paths(&dir, &[t], "ai", "two").is_err());
        assert_eq!(head(&dir, "t/a").0, "one");
        assert_eq!(staged(), ["t/a", "t/b"]);

        fs::remove_file(&held).unwrap();
        commit_paths(&dir, &[t], "ai", "two").unwrap();
        assert_eq!(head(&dir, "t/b"), ("two".into(), "2\n".into()));
        assert_eq!(staged(), ["t/a", "t/b"]);
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
