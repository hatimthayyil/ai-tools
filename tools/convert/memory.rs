//! Moving the user's memory from where ai kept it before, a bare git repository of its own at
//! `~/.ai/memory` holding the ref beside this machine's files, into the user's repository,
//! `~/ai`, as the same ref with its whole history. This machine's files stay in `~/.ai/memory`;
//! git's move aside to `~/.ai/memory-converted.git`, still a repository, so no older ai finds a
//! memory at the old place and writes to it.
//!
//! The memory is first fetched into a temporary repository and read there through `ai`'s own
//! reader. Its objects are then fetched into `~/ai` and compared with the old repository's, this
//! machine's record is written, the ref is made, and the old repository moves aside. A run cut
//! short before the ref is made has changed no memory, and is made again from the start; one cut
//! short after it is finished by the next run, which the record lets it recognise.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use ai_tools::{
    config::Homes,
    file::{self, AtPath, pretty},
    memory::{REF, Site, Store, plural},
    repo,
};
use anyhow::{Context, Result, anyhow, bail};
use gix::{ObjectId, lock::acquire::Fail};
use jiff::Timestamp;

use super::{CONVERTED, Options, Record, converted_from, sync, write_record};

/// This machine's record of the memory's conversion, in [`CONVERTED`].
const RECORD: &str = "memory.json";

/// Where the old repository's own files go, in `~/.ai`.
const ASIDE: &str = "memory-converted.git";

/// The lock a memory's compactor holds, in the directory of this machine's files.
const COMPACTOR: &str = "compact.lock";

/// What this writes into the lock of the old repository's ref, so a later run knows a lock an
/// earlier one left behind from one a writer holds.
const MARK: &[u8] = b"ai convert\n";

/// git's own entries in a bare repository, in the order they move aside: without the first no
/// program opens the repository, and the refs go last, the ref's lock with them, so a writer
/// that opened the repository earlier finds the ref locked until it is gone.
const GIT: [&str; 14] = [
    "HEAD",
    "config",
    "description",
    "hooks",
    "info",
    "branches",
    "logs",
    "objects",
    "packed-refs",
    "shallow",
    "FETCH_HEAD",
    "ORIG_HEAD",
    "gc.log",
    "refs",
];

/// What converting the memory found to do, or did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No memory where ai kept it before.
    Nothing,
    /// Converted earlier on this machine; a conversion cut short is finished.
    Done { to: PathBuf },
    /// Converted, or with `dry_run` would be.
    Converted(Report),
}

/// What a conversion made of the memory.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Where the old repository's own files are now.
    pub aside: PathBuf,
    pub dry_run: bool,
    pub messages: u64,
    /// The commits of the ref's history, all of them carried over.
    pub commits: u64,
}

/// The places a conversion reads and writes.
struct Places {
    /// `~/.ai/memory`: the old repository, and this machine's files of the memory.
    old: PathBuf,
    aside: PathBuf,
    /// `~/ai`.
    repo: PathBuf,
    record: PathBuf,
}

impl Places {
    fn of(homes: &Homes) -> Places {
        let site = Site::User(homes.clone());
        Places {
            old: site.dir(),
            aside: homes.machine.join(ASIDE),
            repo: site.repo().to_path_buf(),
            record: homes.machine.join(CONVERTED).join(RECORD),
        }
    }
}

/// Moves the user's memory into `homes`' repository.
pub fn convert(homes: &Homes, opts: Options) -> Result<Outcome> {
    let p = Places::of(homes);
    let recorded = converted_from(&p.record, &p.old)?;
    let new = new_tip(&p.repo)?;
    if old_tip(&p.old)?.is_none() {
        return match (new, recorded) {
            (Some(_), true) => {
                if !opts.dry_run {
                    aside(&p)?;
                }
                Ok(Outcome::Done { to: p.repo })
            }
            (None, true) if GIT.iter().any(|name| p.old.join(name).exists()) => bail!(
                "{} holds part of the repository a conversion was moving to {}, and {} holds \
                 no memory. Nothing was changed.",
                pretty(&p.old),
                pretty(&p.aside),
                pretty(&p.repo)
            ),
            _ => Ok(Outcome::Nothing),
        };
    }
    match (new, recorded) {
        (None, _) => fresh(&p, opts),
        (Some(_), true) => finish(&p, opts),
        (Some(_), false) => bail!(
            "{} already holds a memory, and {} still holds one: a memory moves only into a \
             repository without one. Neither was changed.",
            pretty(&p.repo),
            pretty(&p.old)
        ),
    }
}

/// Converts the memory in the old repository, which `~/ai` holds nothing of.
fn fresh(p: &Places, opts: Options) -> Result<Outcome> {
    let _compactor = compactor(&p.old, opts.dry_run)?;
    let _writer = writer(&p.old, opts.dry_run)?;
    let tip = old_tip(&p.old)?
        .with_context(|| format!("{} no longer holds a memory.", pretty(&p.old)))?;
    repo::ensurable(&p.repo)?;
    let unconverted = || {
        format!(
            "Cannot move the memory in {}; nothing was converted.",
            pretty(&p.old)
        )
    };
    let (messages, commits) = rehearse(&p.old, tip).with_context(unconverted)?;
    let report = Report {
        from: p.old.clone(),
        to: p.repo.clone(),
        aside: p.aside.clone(),
        dry_run: opts.dry_run,
        messages,
        commits,
    };
    if opts.dry_run {
        return Ok(Outcome::Converted(report));
    }
    repo::ensure(&p.repo)?;
    let git_dir = repo::git_dir(&p.repo);
    bring(&git_dir, &p.old, tip).with_context(unconverted)?;
    sync(&git_dir)?;
    write_record(
        &p.record,
        &Record {
            from: p.old.clone(),
            to: p.repo.clone(),
            at: Timestamp::now(),
        },
    )?;
    point(&git_dir, tip).with_context(unconverted)?;
    aside(p)?;
    Ok(Outcome::Converted(report))
}

/// Finishes a conversion cut short after it made the ref in `~/ai`, while the old repository is
/// still whole: once `~/ai`'s memory is found to hold every commit of the old one, the old
/// repository moves aside.
fn finish(p: &Places, opts: Options) -> Result<Outcome> {
    let _compactor = compactor(&p.old, opts.dry_run)?;
    let _writer = writer(&p.old, opts.dry_run)?;
    let (old, new) = (old_tip(&p.old)?, new_tip(&p.repo)?);
    let held = match (old, new) {
        (Some(old), Some(new)) => contains(&repo::git_dir(&p.repo), new, old)?,
        _ => false,
    };
    if !held {
        bail!(
            "{} holds memories that the memory in {} lacks, written since a conversion was cut \
             short. Neither was changed.",
            pretty(&p.old),
            pretty(&p.repo)
        );
    }
    if !opts.dry_run {
        aside(p)?;
    }
    Ok(Outcome::Done { to: p.repo.clone() })
}

/// The commit the memory's ref names in the repository `repo`; `None` without the ref.
fn tip(repo: &gix::Repository) -> Result<Option<ObjectId>> {
    Ok(repo
        .try_find_reference(REF)?
        .and_then(|r| r.try_id().map(|id| id.detach())))
}

/// The commit of the memory in the old repository at `old`: a bare repository holding the ref,
/// which is what ai refuses the user's memory for until it is converted.
fn old_tip(old: &Path) -> Result<Option<ObjectId>> {
    match gix::open(old).ok().filter(|r| r.is_bare()) {
        Some(r) => tip(&r),
        None => Ok(None),
    }
}

/// The commit of the memory in the user's repository at `repo`; `None` with no repository there.
fn new_tip(repo: &Path) -> Result<Option<ObjectId>> {
    if !repo::exists(repo) {
        return Ok(None);
    }
    tip(&gix::open(repo).with_context(|| format!("Cannot open {}.", pretty(repo)))?)
}

/// The compactor's lock of the memory at `old`, held so none summarises while it moves; an error
/// while a compactor holds it. With `dry_run` it is only looked at.
fn compactor(old: &Path, dry_run: bool) -> Result<Option<file::Lock>> {
    let lock = old.join(COMPACTOR);
    let free = match dry_run {
        true => (!file::held(&lock)?).then_some(None),
        false => file::try_lock(&lock)?.map(Some),
    };
    free.with_context(|| {
        format!(
            "An ai is summarising your memory ({} is held). Let it finish, then try again.",
            pretty(&lock)
        )
    })
}

/// git's lock of the old repository's ref, held so no ai moves the ref while its memory moves;
/// an error while a writer holds it. A lock an earlier run of this left behind is taken over:
/// the compactor's lock, held by now, shows that run is over. With `dry_run` the lock is only
/// looked at.
fn writer(old: &Path, dry_run: bool) -> Result<Option<gix::lock::File>> {
    let at = old.join(REF);
    let lock = old.join(format!("{REF}.lock"));
    let ours = fs::read(&lock).is_ok_and(|b| b == MARK);
    let held = || {
        anyhow!(
            "An ai is writing your memory ({} is held). Let it finish, then try again.",
            pretty(&lock)
        )
    };
    if dry_run {
        return match lock.exists() && !ours {
            true => Err(held()),
            false => Ok(None),
        };
    }
    if ours {
        fs::remove_file(&lock).at(&lock)?;
    }
    match gix::lock::File::acquire_to_update_resource(
        &at,
        Fail::Immediately,
        Some(old.to_path_buf()),
    ) {
        Ok(mut l) => {
            l.with_mut(|f| f.write_all(MARK)).at(&lock)?;
            Ok(Some(l))
        }
        Err(_) if lock.exists() => Err(held()),
        Err(e) => bail!("Cannot lock {}: {e}", pretty(&at)),
    }
}

/// Fetches the memory ending at `tip` from `old` into a temporary repository and reads it there
/// as `ai memory` will; its messages and commits.
fn rehearse(old: &Path, tip: ObjectId) -> Result<(u64, u64)> {
    let tmp = tempfile::Builder::new()
        .prefix("ai-convert-")
        .tempdir()
        .context("Cannot make a temporary directory.")?;
    gix::init_bare(tmp.path())?;
    bring(tmp.path(), old, tip)?;
    point(tmp.path(), tip)?;
    let messages = Store::open(&Site::whole(tmp.path()))?
        .snapshot()?
        .log_len()?;
    let count = run(
        repo::git(tmp.path()).args(["rev-list", "--count", &tip.to_string()]),
        "rev-list",
    )?;
    let commits = String::from_utf8_lossy(&count)
        .trim()
        .parse()
        .context("git rev-list --count printed no number.")?;
    Ok((messages, commits))
}

/// Fetches the memory's ref from the repository `old` into the git directory `git_dir`, naming
/// no ref there, and checks that `git_dir` then holds every object `tip` reaches in `old`. The
/// fetch starts no maintenance: git's would pack `git_dir`'s refs.
fn bring(git_dir: &Path, old: &Path, tip: ObjectId) -> Result<()> {
    run(
        repo::git(git_dir)
            .args([
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                "--no-auto-maintenance",
            ])
            .arg(old)
            .arg(REF),
        "fetch",
    )?;
    if objects(git_dir, tip)? != objects(old, tip)? {
        bail!(
            "{} does not hold every object of the memory in {}.",
            pretty(git_dir),
            pretty(old)
        );
    }
    Ok(())
}

/// Every object `tip` reaches in the git directory `git_dir`, as `git rev-list` lists them; an
/// error if one is missing.
fn objects(git_dir: &Path, tip: ObjectId) -> Result<Vec<u8>> {
    run(
        repo::git(git_dir).args(["rev-list", "--objects", &tip.to_string()]),
        "rev-list",
    )
}

/// Makes the memory's ref in the git directory `git_dir`, at `tip`; an error if it has one.
fn point(git_dir: &Path, tip: ObjectId) -> Result<()> {
    let none = ObjectId::null(tip.kind()).to_string();
    run(
        repo::git(git_dir).args(["update-ref", REF, &tip.to_string(), &none]),
        "update-ref",
    )?;
    let made = tip_at(git_dir)?;
    if made != Some(tip) {
        bail!(
            "{REF} in {} is not the commit it was made at.",
            pretty(git_dir)
        );
    }
    Ok(())
}

/// The commit the memory's ref names in the git directory `git_dir`.
fn tip_at(git_dir: &Path) -> Result<Option<ObjectId>> {
    tip(&gix::open(git_dir).with_context(|| format!("Cannot open {}.", pretty(git_dir)))?)
}

/// Whether the history of `new` in the git directory `git_dir` holds the commit `old`.
fn contains(git_dir: &Path, new: ObjectId, old: ObjectId) -> Result<bool> {
    let status = repo::git(git_dir)
        .args(["merge-base", "--is-ancestor"])
        .args([old.to_string(), new.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(no_git)?;
    Ok(status.success())
}

/// Runs the git command `c`, `what` by name; what it printed, or what it said in failing.
fn run(c: &mut Command, what: &str) -> Result<Vec<u8>> {
    let out = c.stdin(Stdio::null()).output().map_err(no_git)?;
    if !out.status.success() {
        let said: Vec<&str> = std::str::from_utf8(&out.stderr)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        bail!("git {what} failed: {}", said.join(" "));
    }
    Ok(out.stdout)
}

/// Why git could not start.
fn no_git(e: io::Error) -> anyhow::Error {
    match e.kind() {
        io::ErrorKind::NotFound => {
            anyhow!("Moving a memory needs git, and no git program was found.")
        }
        _ => anyhow!("Cannot start git: {e}"),
    }
}

/// Moves git's own entries of the old repository aside, those still there, and removes this
/// conversion's lock of its ref, which went with them. What is left in `~/.ai/memory` is this
/// machine's files, and no repository.
fn aside(p: &Places) -> Result<()> {
    let mut moved = false;
    for name in GIT {
        let from = p.old.join(name);
        if fs::symlink_metadata(&from).is_err() {
            continue;
        }
        fs::create_dir_all(&p.aside).at(&p.aside)?;
        let to = p.aside.join(name);
        file::rename_new(&from, &to)
            .with_context(|| format!("Cannot move {} to {}.", pretty(&from), pretty(&to)))?;
        moved = true;
    }
    let lock = p.aside.join(format!("{REF}.lock"));
    match fs::remove_file(&lock) {
        Ok(()) => moved = true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).at(&lock),
    }
    match moved {
        true => sync(&p.aside),
        false => Ok(()),
    }
}

/// The lines `ai convert` prints of the memory; none when there was nothing to convert.
pub fn describe(outcome: &Outcome) -> Vec<String> {
    match outcome {
        Outcome::Nothing => Vec::new(),
        Outcome::Done { to } => vec![format!("memory: already converted to {}.", pretty(to))],
        Outcome::Converted(r) => {
            let mut out = vec![format!(
                "memory: {} {} ({}) from {} to {}.",
                match r.dry_run {
                    true => "would convert",
                    false => "converted",
                },
                plural(r.messages, "message"),
                plural(r.commits, "commit"),
                pretty(&r.from),
                pretty(&r.to),
            )];
            if !r.dry_run {
                out.push(format!(
                    "  {} keeps this machine's files; the old repository is now {}, no longer \
                     read, and once you have checked the conversion, yours to delete.",
                    pretty(&r.from),
                    pretty(&r.aside)
                ));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use ai_tools::memory::{Kind, Message};
    use tempfile::TempDir;

    use super::*;

    /// A home directory of its own, with the places a conversion reads and writes there.
    struct Home {
        _dir: TempDir,
        homes: Homes,
        p: Places,
    }

    impl Home {
        fn new() -> Home {
            let dir = TempDir::new().unwrap();
            let homes = Homes::at(dir.path());
            let p = Places::of(&homes);
            Home {
                _dir: dir,
                homes,
                p,
            }
        }

        /// The user's memory, in `~/ai`.
        fn user(&self) -> Site {
            Site::User(self.homes.clone())
        }

        /// Makes a memory as ai kept it before, a repository at `~/.ai/memory` with this machine's
        /// files beside git's, holding `notes`; its ref's commit.
        fn old(&self, notes: &[&str]) -> ObjectId {
            let (store, _) = Store::create(&Site::whole(&self.p.old)).unwrap();
            for n in notes {
                note(&store, n);
            }
            fs::create_dir(self.p.old.join("calls")).unwrap();
            for (name, bytes) in MACHINE {
                fs::write(self.p.old.join(name), bytes).unwrap();
            }
            store.head().unwrap().unwrap()
        }

        fn convert(&self) -> Result<Outcome> {
            convert(&self.homes, Options { dry_run: false })
        }

        fn dry_run(&self) -> Result<Outcome> {
            convert(&self.homes, Options { dry_run: true })
        }

        /// The steps of a conversion up to its record, as a run cut short there leaves them: the
        /// objects in `~/ai`, the record, and its lock of the old ref.
        fn cut_short_before_the_ref(&self, tip: ObjectId) {
            repo::ensure(&self.p.repo).unwrap();
            bring(&repo::git_dir(&self.p.repo), &self.p.old, tip).unwrap();
            let record = Record {
                from: self.p.old.clone(),
                to: self.p.repo.clone(),
                at: Timestamp::now(),
            };
            write_record(&self.p.record, &record).unwrap();
            fs::write(self.p.old.join(format!("{REF}.lock")), MARK).unwrap();
        }

        /// As [`Home::cut_short_before_the_ref`], with the ref made in `~/ai`.
        fn cut_short_after_the_ref(&self, tip: ObjectId) {
            self.cut_short_before_the_ref(tip);
            point(&repo::git_dir(&self.p.repo), tip).unwrap();
        }

        /// Checks the memory is converted whole: `~/ai` holds `notes` and takes another, this
        /// machine's files are as [`Home::old`] wrote them, and the old repository is aside,
        /// ending at `tip`, with nothing of git's left where an older ai looks.
        fn assert_converted(&self, tip: ObjectId, notes: &[&str]) {
            let store = Store::open(&self.user()).unwrap();
            assert_eq!(texts(&store), notes);
            assert!(
                contains(
                    &repo::git_dir(&self.p.repo),
                    store.head().unwrap().unwrap(),
                    tip
                )
                .unwrap()
            );
            note(&store, "one more");
            assert_eq!(texts(&store).len(), notes.len() + 1);
            for (name, bytes) in MACHINE {
                assert_eq!(fs::read(self.p.old.join(name)).unwrap(), bytes, "{name}");
            }
            assert!(self.p.old.join("origin").is_file());
            for name in GIT {
                assert!(!self.p.old.join(name).exists(), "{name} is still there");
            }
            let older = Store::open(&Site::whole(&self.p.old)).err().unwrap();
            assert!(older.to_string().starts_with("No memory at"), "{older}");
            assert_eq!(old_tip(&self.p.aside).unwrap(), Some(tip));
            assert_eq!(names(&self.p.aside.join("refs/ai")), ["memory"]);
        }

        /// Checks nothing was converted: the old repository is a memory ending at `tip`, with
        /// no lock of this left on its ref.
        fn assert_unconverted(&self, tip: ObjectId) {
            assert_eq!(old_tip(&self.p.old).unwrap(), Some(tip));
            assert!(!self.p.old.join(format!("{REF}.lock")).exists());
            assert!(!self.p.aside.exists());
        }
    }

    /// This machine's files of a memory, besides its origin, with what [`Home::old`] writes in them.
    const MACHINE: [(&str, &[u8]); 4] = [
        ("calls/1.json", b"{}\n"),
        ("usage.jsonl", b"{\"cmd\":\"note\"}\n"),
        ("summarise.log", b"built\n"),
        ("nap.log", b"napped\n"),
    ];

    fn note(store: &Store, text: &str) {
        store
            .append("note", &[Message::new(Kind::Note, text)])
            .unwrap();
    }

    fn texts(store: &Store) -> Vec<String> {
        let mut out = Vec::new();
        let snap = store.snapshot().unwrap();
        snap.scan(u64::MAX, |_, m| {
            out.push(m.text);
            Ok(())
        })
        .unwrap();
        out
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Every file under `dir` with its bytes, by its path.
    fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            match p.is_dir() {
                true => out.extend(files(&p)),
                false => out.push((p.clone(), fs::read(&p).unwrap())),
            }
        }
        out.sort();
        out
    }

    fn commits(git_dir: &Path, tip: ObjectId) -> u64 {
        let out = run(
            repo::git(git_dir).args(["rev-list", "--count", &tip.to_string()]),
            "rev-list",
        )
        .unwrap();
        String::from_utf8(out).unwrap().trim().parse().unwrap()
    }

    fn said(r: Result<Outcome>) -> String {
        ai_tools::error::line(&r.unwrap_err())
    }

    #[test]
    fn a_memory_moves_into_the_users_repository_with_its_whole_history() {
        let h = Home::new();
        let tip = h.old(&["one", "two", "three"]);
        let history = commits(&h.p.old, tip);
        assert!(history >= 3);

        let Outcome::Converted(r) = h.convert().unwrap() else {
            panic!("not converted");
        };
        assert_eq!(
            r,
            Report {
                from: h.p.old.clone(),
                to: h.p.repo.clone(),
                aside: h.p.aside.clone(),
                dry_run: false,
                messages: 3,
                commits: history,
            }
        );
        assert_eq!(new_tip(&h.p.repo).unwrap(), Some(tip));
        assert_eq!(commits(&repo::git_dir(&h.p.repo), tip), history);
        let origin = fs::read(h.p.old.join("origin")).unwrap();
        h.assert_converted(tip, &["one", "two", "three"]);
        assert_eq!(fs::read(h.p.old.join("origin")).unwrap(), origin);
        assert_eq!(
            Store::open(&h.user()).unwrap().origin().as_bytes(),
            origin.trim_ascii()
        );
    }

    #[test]
    fn a_dry_run_changes_nothing_and_tells_what_would_move() {
        let h = Home::new();
        let tip = h.old(&["one", "two"]);
        let before = files(&h.homes.home);

        let Outcome::Converted(r) = h.dry_run().unwrap() else {
            panic!("no conversion told of");
        };
        assert!(r.dry_run);
        assert_eq!((r.messages, r.commits), (2, commits(&h.p.old, tip)));
        assert_eq!(files(&h.homes.home), before);
        assert!(!h.p.repo.exists());
        h.assert_unconverted(tip);
    }

    #[test]
    fn a_converted_memory_is_found_converted_and_left_as_it_is() {
        let h = Home::new();
        h.old(&["one"]);
        h.convert().unwrap();
        let before = files(&h.homes.home);

        let done = Outcome::Done {
            to: h.p.repo.clone(),
        };
        assert_eq!(h.dry_run().unwrap(), done);
        assert_eq!(h.convert().unwrap(), done);
        assert_eq!(files(&h.homes.home), before);
    }

    #[test]
    fn a_home_without_a_memory_where_ai_kept_it_has_nothing_to_convert() {
        let h = Home::new();
        assert_eq!(h.dry_run().unwrap(), Outcome::Nothing);
        assert_eq!(h.convert().unwrap(), Outcome::Nothing);
        assert_eq!(names(&h.homes.home), [] as [&str; 0]);

        let (store, _) = Store::create(&h.user()).unwrap();
        note(&store, "made here");
        assert_eq!(h.convert().unwrap(), Outcome::Nothing);
        assert_eq!(texts(&store), ["made here"]);
    }

    #[test]
    fn a_memory_being_summarised_is_refused() {
        let h = Home::new();
        let tip = h.old(&["one"]);
        let lock = h.p.old.join(COMPACTOR);
        let held = file::try_lock(&lock).unwrap().unwrap();
        let refused = format!(
            "An ai is summarising your memory ({} is held). Let it finish, then try again.",
            pretty(&lock)
        );

        assert_eq!(said(h.dry_run()), refused);
        assert_eq!(said(h.convert()), refused);
        h.assert_unconverted(tip);
        assert!(!h.p.repo.exists());

        drop(held);
        assert!(matches!(h.convert().unwrap(), Outcome::Converted(_)));
    }

    #[test]
    fn a_memory_being_written_is_refused_and_a_lock_of_a_run_cut_short_is_taken_over() {
        let h = Home::new();
        let tip = h.old(&["one"]);
        let lock = h.p.old.join(format!("{REF}.lock"));
        fs::write(&lock, "").unwrap();
        let refused = format!(
            "An ai is writing your memory ({} is held). Let it finish, then try again.",
            pretty(&lock)
        );

        assert_eq!(said(h.dry_run()), refused);
        assert_eq!(said(h.convert()), refused);
        assert_eq!(fs::read(&lock).unwrap(), b"");
        assert_eq!(old_tip(&h.p.old).unwrap(), Some(tip));
        assert!(!h.p.repo.exists());

        fs::write(&lock, MARK).unwrap();
        assert!(matches!(h.dry_run().unwrap(), Outcome::Converted(_)));
        assert_eq!(fs::read(&lock).unwrap(), MARK);
        assert!(matches!(h.convert().unwrap(), Outcome::Converted(_)));
        h.assert_converted(tip, &["one"]);
    }

    #[test]
    fn a_repository_that_already_holds_a_memory_is_refused_naming_both() {
        let h = Home::new();
        let (new, _) = Store::create(&h.user()).unwrap();
        note(&new, "made here");
        let new_tip = new.head().unwrap().unwrap();
        fs::remove_dir_all(&h.p.old).unwrap();
        let tip = h.old(&["an old one"]);
        let refused = format!(
            "{} already holds a memory, and {} still holds one: a memory moves only into a \
             repository without one. Neither was changed.",
            pretty(&h.p.repo),
            pretty(&h.p.old)
        );

        assert_eq!(said(h.dry_run()), refused);
        assert_eq!(said(h.convert()), refused);
        h.assert_unconverted(tip);
        assert_eq!(super::new_tip(&h.p.repo).unwrap(), Some(new_tip));
        assert!(!h.p.record.exists());
    }

    #[test]
    fn the_users_repository_keeps_what_it_holds_and_a_folder_that_is_none_is_refused() {
        let h = Home::new();
        let tip = h.old(&["one"]);
        fs::create_dir_all(h.p.repo.join("projects")).unwrap();
        fs::write(h.p.repo.join("projects/thread"), "a thread\n").unwrap();
        let refused = said(h.dry_run());
        assert!(
            refused.contains("holds files but is not a git repository"),
            "{refused}"
        );
        assert_eq!(said(h.convert()), refused);
        h.assert_unconverted(tip);
        assert!(!repo::exists(&h.p.repo));

        repo::create(&h.p.repo).unwrap();
        repo::commit(&h.p.repo, "ai", "a thread").unwrap();
        let head = gix::open(&h.p.repo).unwrap().head_id().unwrap().detach();
        assert!(matches!(h.convert().unwrap(), Outcome::Converted(_)));
        h.assert_converted(tip, &["one"]);
        assert_eq!(
            gix::open(&h.p.repo).unwrap().head_id().unwrap().detach(),
            head
        );
        assert_eq!(
            fs::read(h.p.repo.join("projects/thread")).unwrap(),
            b"a thread\n"
        );
    }

    #[test]
    fn a_run_cut_short_before_the_ref_is_made_starts_again() {
        let h = Home::new();
        let tip = h.old(&["one", "two"]);
        h.cut_short_before_the_ref(tip);

        assert!(matches!(h.dry_run().unwrap(), Outcome::Converted(_)));
        assert_eq!(new_tip(&h.p.repo).unwrap(), None);
        assert!(matches!(h.convert().unwrap(), Outcome::Converted(_)));
        h.assert_converted(tip, &["one", "two"]);
    }

    #[test]
    fn a_run_cut_short_after_the_ref_is_made_is_finished() {
        let done = |h: &Home| Outcome::Done {
            to: h.p.repo.clone(),
        };

        let h = Home::new();
        let tip = h.old(&["one", "two"]);
        h.cut_short_after_the_ref(tip);
        let before = files(&h.homes.home);
        assert_eq!(h.dry_run().unwrap(), done(&h));
        assert_eq!(files(&h.homes.home), before);
        assert_eq!(h.convert().unwrap(), done(&h));
        h.assert_converted(tip, &["one", "two"]);

        let h = Home::new();
        let tip = h.old(&["one", "two"]);
        h.cut_short_after_the_ref(tip);
        fs::create_dir(&h.p.aside).unwrap();
        for name in ["HEAD", "objects"] {
            fs::rename(h.p.old.join(name), h.p.aside.join(name)).unwrap();
        }
        assert_eq!(h.convert().unwrap(), done(&h));
        h.assert_converted(tip, &["one", "two"]);

        let h = Home::new();
        let tip = h.old(&["one", "two"]);
        h.cut_short_after_the_ref(tip);
        note(&Store::open(&h.user()).unwrap(), "written since");
        assert_eq!(h.convert().unwrap(), done(&h));
        h.assert_converted(tip, &["one", "two", "written since"]);
    }

    #[test]
    fn a_repository_half_moved_aside_whose_memory_is_gone_from_the_users_is_refused() {
        let h = Home::new();
        let tip = h.old(&["one"]);
        h.cut_short_after_the_ref(tip);
        fs::create_dir(&h.p.aside).unwrap();
        fs::rename(h.p.old.join("HEAD"), h.p.aside.join("HEAD")).unwrap();
        let git_dir = repo::git_dir(&h.p.repo);
        run(
            repo::git(&git_dir).args(["update-ref", "-d", REF]),
            "update-ref",
        )
        .unwrap();
        let before = files(&h.homes.home);

        let refused = format!(
            "{} holds part of the repository a conversion was moving to {}, and {} holds no \
             memory. Nothing was changed.",
            pretty(&h.p.old),
            pretty(&h.p.aside),
            pretty(&h.p.repo)
        );
        assert_eq!(said(h.dry_run()), refused);
        assert_eq!(said(h.convert()), refused);
        assert_eq!(files(&h.homes.home), before);
    }

    #[test]
    fn an_old_memory_written_since_a_run_was_cut_short_is_refused() {
        let h = Home::new();
        let tip = h.old(&["one"]);
        h.cut_short_after_the_ref(tip);
        fs::remove_file(h.p.old.join(format!("{REF}.lock"))).unwrap();
        let old = Store::open(&Site::whole(&h.p.old)).unwrap();
        note(&old, "written by an older ai");
        let newer = old.head().unwrap().unwrap();
        let refused = format!(
            "{} holds memories that the memory in {} lacks, written since a conversion was cut \
             short. Neither was changed.",
            pretty(&h.p.old),
            pretty(&h.p.repo)
        );

        assert_eq!(said(h.dry_run()), refused);
        assert_eq!(said(h.convert()), refused);
        h.assert_unconverted(newer);
        assert_eq!(new_tip(&h.p.repo).unwrap(), Some(tip));
    }

    #[test]
    fn a_memory_missing_an_object_is_not_converted() {
        let h = Home::new();
        let tip = h.old(&["one", "two"]);
        let tree = run(
            repo::git(&h.p.old).args(["rev-parse", &format!("{tip}^{{tree}}")]),
            "rev-parse",
        )
        .unwrap();
        let tree = String::from_utf8(tree).unwrap();
        let (fan, rest) = tree.trim().split_at(2);
        fs::remove_file(h.p.old.join("objects").join(fan).join(rest)).unwrap();

        for r in [h.dry_run(), h.convert()] {
            let refused = said(r);
            assert!(
                refused.starts_with(&format!(
                    "Cannot move the memory in {}; nothing was converted.",
                    pretty(&h.p.old)
                )),
                "{refused}"
            );
        }
        h.assert_unconverted(tip);
        assert!(!h.p.repo.exists());
        assert!(!h.p.record.exists());
    }

    #[test]
    fn each_outcome_is_told_in_its_own_words() {
        let r = |dry_run| {
            Outcome::Converted(Report {
                from: "/h/.ai/memory".into(),
                to: "/h/ai".into(),
                aside: "/h/.ai/memory-converted.git".into(),
                dry_run,
                messages: 2039,
                commits: 1,
            })
        };
        assert_eq!(describe(&Outcome::Nothing), [] as [&str; 0]);
        assert_eq!(
            describe(&Outcome::Done { to: "/h/ai".into() }),
            ["memory: already converted to /h/ai."]
        );
        assert_eq!(
            describe(&r(true)),
            ["memory: would convert 2039 messages (1 commit) from /h/.ai/memory to /h/ai."]
        );
        assert_eq!(
            describe(&r(false)),
            [
                "memory: converted 2039 messages (1 commit) from /h/.ai/memory to /h/ai.",
                "  /h/.ai/memory keeps this machine's files; the old repository is now \
                 /h/.ai/memory-converted.git, no longer read, and once you have checked the \
                 conversion, yours to delete."
            ]
        );
    }
}
