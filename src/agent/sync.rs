use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{
    agent::{
        claude,
        record::{Binding, State, Track, check_id},
        source,
        store::{Added, Store, Thread, Transcript},
    },
    config::Homes,
};

/// The outcome of [`sync`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Synced {
    /// The session is attached to no thread of the project, or there is no project.
    NotAttached,
    Attached {
        thread: String,
        added: Added,
    },
}

/// The outcome of [`Store::attach`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attached {
    pub thread: String,
    pub added: Added,
}

/// Tells each warning of `added`, that a file no longer holds what a thread read of it, on
/// stderr.
fn tell(added: &Added) {
    for l in &added.unheld {
        eprintln!("{l}");
    }
}

/// The session's transcript at `path`, read by its harness's adapter; empty if it does not
/// exist yet.
pub fn read(harness: &str, session: &str, path: Option<&Path>) -> Result<Transcript> {
    let Some(path) = path else {
        return Ok(Transcript::default());
    };
    let files = source::capture(path)?;
    if files.is_empty() {
        return Ok(Transcript::default());
    }
    let name = Path::new(
        path.file_name()
            .ok_or_else(|| anyhow::anyhow!("A transcript needs a file name."))?,
    );
    let dir = path
        .parent()
        .context("A transcript needs a parent directory.")?;
    match harness {
        claude::HARNESS => {
            let items = files
                .get(name)
                .map_or_else(Vec::new, |bytes| claude::parse_bytes(session, bytes));
            let agents = claude::subagents_from(dir, &files, &name.with_extension(""), session)?;
            let rewritten = files
                .keys()
                .filter(|f| claude::rewrites(f))
                .cloned()
                .collect();
            Ok(Transcript {
                items,
                agents,
                files,
                rewritten,
                dir: dir.to_path_buf(),
                main: name.to_path_buf(),
            })
        }
        _ => bail!("ai agent cannot read {harness} sessions."),
    }
}

/// Ingests the new entries of an attached session; a session attached to no thread of the
/// project at `dir`, its agents in `homes`, is [`Synced::NotAttached`].
pub fn sync(
    homes: &Homes,
    dir: &Path,
    harness: &str,
    session: &str,
    transcript: Option<&Path>,
) -> Result<Synced> {
    match Store::find_in(homes, dir)? {
        Some(store) => store.sync(harness, session, transcript),
        None => Ok(Synced::NotAttached),
    }
}

impl Store {
    /// The thread `session` is attached to, with its binding there.
    pub fn attached(&self, harness: &str, session: &str) -> Result<Option<(Thread, Binding)>> {
        let _lock = self.session_lock(harness, session)?;
        let Some(name) = self.owner(harness, session)? else {
            return Ok(None);
        };
        let t = self.thread(&name)?;
        let b = t.snapshot()?.binding(harness, session)?.with_context(|| {
            format!(
                "Session {harness} {session} is attached to {name}, which holds no record of it."
            )
        })?;
        Ok(Some((t, b)))
    }

    /// Ingests the new entries of an attached session, from `transcript` or the path its
    /// binding holds; none if nothing is new.
    pub fn sync(&self, harness: &str, session: &str, transcript: Option<&Path>) -> Result<Synced> {
        self.sync_with(harness, session, transcript, read)
    }

    fn sync_with(
        &self,
        harness: &str,
        session: &str,
        transcript: Option<&Path>,
        read: impl FnOnce(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Synced> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let _lock = self.session_lock(harness, session)?;
        let Some(name) = self.owner(harness, session)? else {
            return Ok(Synced::NotAttached);
        };
        let added = self.catch_up(&name, harness, session, transcript, read)?;
        Ok(Synced::Attached {
            thread: name,
            added,
        })
    }

    /// Ingests the new entries of a session attached to `name`; the caller holds the
    /// session's lock.
    fn catch_up(
        &self,
        name: &str,
        harness: &str,
        session: &str,
        transcript: Option<&Path>,
        read: impl FnOnce(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Added> {
        let thread = self.thread(name)?;
        let old = thread
            .snapshot()?
            .track(harness, session)?
            .unwrap_or(Track {
                harness: harness.into(),
                session: session.into(),
                transcript: None,
                config: None,
                graft: None,
                tip: None,
            });
        let path = transcript
            .map(source::real)
            .or_else(|| old.transcript.clone());
        let t = read(harness, session, path.as_deref())?;
        let mut b = Track {
            transcript: path,
            config: transcript
                .and_then(|p| source::config(harness, p))
                .or_else(|| old.config.clone()),
            ..old.clone()
        };
        let _w = thread.writer()?;
        let added = thread.snapshot()?.ingest(&thread, &mut b, &t)?;
        tell(&added);
        if b != old {
            thread.put_track(&b)?;
        }
        Ok(added)
    }

    /// Attaches a session to agent `name`'s thread and ingests its whole
    /// history. A new binding grafts the session's roots onto the thread's newest item; a
    /// session attached to another thread must be detached explicitly first.
    pub fn attach(
        &self,
        name: &str,
        harness: &str,
        session: &str,
        transcript: Option<&Path>,
    ) -> Result<Attached> {
        self.attach_with(name, harness, session, transcript, read)
    }

    fn attach_with(
        &self,
        name: &str,
        harness: &str,
        session: &str,
        transcript: Option<&Path>,
        read: impl FnOnce(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Attached> {
        self.attach_after(name, harness, session, transcript, None, read)
    }

    /// Attaches as [`Store::attach`] does; a new binding grafts onto the newest item of session
    /// `after`'s branch when one is given, the session it goes on from. The attachment is
    /// recorded under the writer's lock that the session's entries are written under, so a
    /// rename or removal of the thread, which waits for that lock, finds the session attached.
    fn attach_after(
        &self,
        name: &str,
        harness: &str,
        session: &str,
        transcript: Option<&Path>,
        after: Option<&str>,
        read: impl FnOnce(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Attached> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let target = self.existing(name)?;
        let _lock = self.session_lock(harness, session)?;
        let owner = self.owner(harness, session)?;
        if let Some(current) = owner.as_deref().filter(|&o| o != name) {
            bail!(
                "Session {harness} {session} is attached to {current}. Run ai agent detach --harness {harness} --session {session} before attaching to {name}."
            );
        }
        let bound = target.snapshot()?.track(harness, session)?;
        let path: Option<PathBuf> = transcript
            .map(source::real)
            .or_else(|| bound.as_ref().and_then(|b| b.transcript.clone()));
        let t = read(harness, session, path.as_deref())?;
        let config = transcript.and_then(|p| source::config(harness, p));
        let _w = target.writer()?;
        let snap = target.snapshot()?;
        let mut b = match bound.clone() {
            Some(b) => Track {
                transcript: path.or(b.transcript),
                config: config.or(b.config),
                ..b
            },
            None => Track {
                harness: harness.into(),
                session: session.into(),
                transcript: path,
                config,
                graft: match after {
                    Some(s) => snap.track(harness, s)?.and_then(|t| t.tip.or(t.graft)),
                    None => snap.graft()?,
                },
                tip: None,
            },
        };
        let added = snap.ingest(&target, &mut b, &t)?;
        tell(&added);
        if bound.as_ref() != Some(&b) {
            target.put_track(&b)?;
        }
        if owner.is_none() {
            target.put_event(harness, session, State::Attached)?;
        }
        Ok(Attached {
            thread: name.into(),
            added,
        })
    }

    /// Detaches a session from each thread it is attached to, after ingesting its new entries
    /// there, so nothing written since the last sync is left out; returns the threads, none if
    /// it was attached to none. A session is attached to one thread, and to several only where
    /// a merge of `~/ai` left it so: detaching is what clears that. A failed catch-up still
    /// detaches, then reports the failure.
    pub fn detach(&self, harness: &str, session: &str) -> Result<Vec<String>> {
        self.detach_with(harness, session, read)
    }

    fn detach_with(
        &self,
        harness: &str,
        session: &str,
        read: impl Fn(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Vec<String>> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let _lock = self.session_lock(harness, session)?;
        let names = self.owners(harness, session)?;
        let mut failed = Ok(());
        for name in &names {
            let caught = self.catch_up(name, harness, session, None, &read);
            self.thread(name)?
                .record(harness, session, State::Detached)?;
            failed = failed.and(
                caught
                    .map(drop)
                    .with_context(|| format!("Detached from {name} without its newest entries")),
            );
        }
        failed?;
        Ok(names)
    }

    /// Attaches session `to` to the thread session `from` is attached to, after ingesting
    /// `from`'s new entries: `to` goes on from `from`, as the session a `/clear` starts does, so
    /// its roots graft onto the newest item of `from`'s branch. `from` stays attached. None if
    /// `from` is attached to no thread. A failed catch-up still attaches, then reports the
    /// failure.
    pub fn carry(
        &self,
        harness: &str,
        from: &str,
        to: &str,
        transcript: Option<&Path>,
    ) -> Result<Option<Attached>> {
        self.carry_with(harness, from, to, transcript, read)
    }

    fn carry_with(
        &self,
        harness: &str,
        from: &str,
        to: &str,
        transcript: Option<&Path>,
        read_from: impl FnOnce(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Option<Attached>> {
        check_id(harness, "harness")?;
        check_id(from, "session")?;
        if from == to {
            bail!("Session {harness} {to} cannot go on from itself.");
        }
        let _lock = self.session_lock(harness, from)?;
        let Some(name) = self.owner(harness, from)? else {
            return Ok(None);
        };
        let caught = self.catch_up(&name, harness, from, None, read_from);
        let attached = self.attach_after(&name, harness, to, transcript, Some(from), read)?;
        caught.with_context(|| {
            format!("Attached to {name} without the newest entries of session {from}")
        })?;
        Ok(Some(attached))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, sync::mpsc, thread, time::Duration};

    use super::*;

    /// A project at `dir` with `agents`.
    fn init(dir: &Path, agents: &[&str]) {
        crate::project::Project::init_in(dir, &homes(dir)).unwrap();
        for a in agents {
            open(dir).create(a).unwrap();
        }
    }

    /// The agent store of the project at `dir`.
    fn open(dir: &Path) -> Store {
        Store::find_in(&homes(dir), dir).unwrap().unwrap()
    }

    /// The user's homes for the project at `dir`: in it, never the real ones.
    fn homes(dir: &Path) -> Homes {
        Homes::at(&dir.join("home"))
    }

    fn plain() -> String {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/plain.jsonl");
        fs::read_to_string(fixture).unwrap()
    }

    #[test]
    fn an_older_sync_finishing_last_preserves_newer_progress() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["dev"]);
        let store = open(dir.path());
        let all = plain();
        let prefix: String = all.split_inclusive('\n').take(3).collect();
        let transcript = dir.path().join("s-1.jsonl");
        fs::write(&transcript, &prefix).unwrap();
        store
            .attach("dev", "claude", "s-1", Some(&transcript))
            .unwrap();
        let (captured, observed) = mpsc::channel();
        let (resume, resumed) = mpsc::channel::<()>();
        let path = dir.path().to_path_buf();
        let older = thread::spawn(move || {
            let store = open(&path);
            store
                .sync_with("claude", "s-1", None, |harness, session, path| {
                    let transcript = read(harness, session, path)?;
                    captured.send(()).unwrap();
                    resumed.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(transcript)
                })
                .unwrap()
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap()
            .write_all(&all.as_bytes()[prefix.len()..])
            .unwrap();
        let path = dir.path().to_path_buf();
        let newer = thread::spawn(move || {
            let store = open(&path);
            store.sync("claude", "s-1", None).unwrap()
        });
        resume.send(()).unwrap();
        older.join().unwrap();
        newer.join().unwrap();

        let fresh = tempfile::TempDir::new().unwrap();
        init(fresh.path(), &["dev"]);
        let whole = open(fresh.path());
        whole
            .attach("dev", "claude", "s-1", Some(&transcript))
            .unwrap();
        let expected = whole.thread("dev").unwrap().snapshot().unwrap();
        let after = store.thread("dev").unwrap().snapshot().unwrap();
        assert_eq!(after.items().unwrap(), expected.items().unwrap());
        assert_eq!(
            after.tip("claude", "s-1").unwrap(),
            expected.tip("claude", "s-1").unwrap()
        );
    }

    #[test]
    fn a_sync_under_way_finishes_into_its_thread_before_the_thread_is_renamed() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["dev"]);
        let store = open(dir.path());
        let all = plain();
        let prefix: String = all.split_inclusive('\n').take(3).collect();
        let transcript = dir.path().join("s-1.jsonl");
        fs::write(&transcript, &prefix).unwrap();
        store
            .attach("dev", "claude", "s-1", Some(&transcript))
            .unwrap();
        fs::write(&transcript, &all).unwrap();
        let (paused, observed) = mpsc::channel();
        let (resume, resumed) = mpsc::channel::<()>();
        let path = dir.path().to_path_buf();
        let syncing = thread::spawn(move || {
            open(&path).sync_with("claude", "s-1", None, |harness, session, path| {
                let transcript = read(harness, session, path)?;
                paused.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(transcript)
            })
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let path = dir.path().to_path_buf();
        let renaming = thread::spawn(move || {
            open(&path)
                .rename("dev", "reviewer", |_| {})
                .map(|t| t.name().to_string())
        });
        thread::sleep(Duration::from_millis(200));
        assert!(store.thread("dev").unwrap().exists().unwrap());
        resume.send(()).unwrap();
        let synced = syncing.join().unwrap().unwrap();
        assert!(matches!(synced, Synced::Attached { thread, .. } if thread == "dev"));
        assert_eq!(renaming.join().unwrap().unwrap(), "reviewer");

        let fresh = tempfile::TempDir::new().unwrap();
        init(fresh.path(), &["dev"]);
        let whole = open(fresh.path());
        whole
            .attach("dev", "claude", "s-1", Some(&transcript))
            .unwrap();
        let expected = whole.thread("dev").unwrap().snapshot().unwrap();
        let after = store.thread("reviewer").unwrap().snapshot().unwrap();
        assert_eq!(after.items().unwrap(), expected.items().unwrap());
        let again = store.sync("claude", "s-1", None).unwrap();
        assert!(
            matches!(&again, Synced::Attached { thread, added } if thread == "reviewer" && added.is_empty()),
            "{again:?}"
        );
    }

    #[test]
    fn concurrent_attachments_have_one_owner_and_detach_clears_it() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["one", "two"]);
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for name in ["one", "two"] {
            let path = dir.path().to_path_buf();
            let start = start.clone();
            workers.push(thread::spawn(move || {
                let store = open(&path);
                start.wait();
                store.attach(name, "claude", "s-1", None)
            }));
        }
        start.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let error = results.into_iter().find_map(Result::err).unwrap();
        assert!(error.to_string().contains("Run ai agent detach"));

        let store = open(dir.path());
        let (owner, binding) = store.attached("claude", "s-1").unwrap().unwrap();
        assert!(binding.is_attached());
        assert_eq!(store.detach("claude", "s-1").unwrap(), [owner.name()]);
        assert!(store.attached("claude", "s-1").unwrap().is_none());
        assert!(store.detach("claude", "s-1").unwrap().is_empty());
        let b = owner.snapshot().unwrap().binding("claude", "s-1").unwrap();
        assert!(b.is_some_and(|b| !b.is_attached()));
    }

    #[test]
    fn recorded_source_and_projection_use_the_same_captured_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["dev"]);
        let store = open(dir.path());
        let all = plain();
        let original: String = all.split_inclusive('\n').take(3).collect();
        let path = dir.path().join("s-1.jsonl");
        fs::write(&path, &original).unwrap();

        store
            .attach_with(
                "dev",
                "claude",
                "s-1",
                Some(&path),
                |harness, session, path| {
                    let captured = read(harness, session, path)?;
                    fs::write(path.unwrap(), &all).unwrap();
                    Ok(captured)
                },
            )
            .unwrap();

        let snapshot = store.thread("dev").unwrap().snapshot().unwrap();
        assert_eq!(snapshot.len().unwrap(), 1);
        let versions = snapshot.versions().unwrap();
        let v = versions
            .iter()
            .find(|v| v.read.path == source::real(&path))
            .unwrap();
        assert_eq!(
            (v.read.len, v.read.sha256.as_str()),
            (
                original.len() as u64,
                crate::file::sha256(original.as_bytes()).as_str()
            ),
            "the recorded bytes must be the parsed observation's"
        );
        assert!(!store.dir().join("threads/dev/sources").exists());
        assert_ne!(fs::read(path).unwrap(), original.as_bytes());
    }

    #[test]
    fn detach_keeps_entries_written_since_the_last_sync() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["dev"]);
        let store = open(dir.path());
        let all = plain();
        let transcript = dir.path().join("s-1.jsonl");
        let prefix: String = all.split_inclusive('\n').take(3).collect();
        fs::write(&transcript, prefix).unwrap();
        store
            .attach("dev", "claude", "s-1", Some(&transcript))
            .unwrap();
        let len = |c: &Store| c.thread("dev").unwrap().snapshot().unwrap().len().unwrap();
        let before = len(&store);
        fs::write(&transcript, &all).unwrap();
        assert_eq!(store.detach("claude", "s-1").unwrap(), ["dev"]);
        assert!(len(&store) > before);
        assert!(store.attached("claude", "s-1").unwrap().is_none());
    }

    #[test]
    fn only_create_makes_an_agent() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &[]);
        let store = open(dir.path());
        let e = store.attach("dev", "claude", "s-1", None).unwrap_err();
        assert_eq!(
            e.to_string(),
            "No agent dev in this project. Create it: ai agent new dev"
        );
        assert!(store.threads().unwrap().is_empty());
        assert!(store.attached("claude", "s-1").unwrap().is_none());
        store.create("dev").unwrap();
        let Err(e) = store.create("dev") else {
            panic!("created twice")
        };
        assert_eq!(e.to_string(), "Agent dev already exists in this project.");
        store.attach("dev", "claude", "s-1", None).unwrap();
    }

    #[test]
    fn a_failed_catch_up_still_detaches() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["dev"]);
        let store = open(dir.path());
        store.attach("dev", "claude", "s-1", None).unwrap();
        let e = store
            .detach_with("claude", "s-1", |_, _, _| bail!("unreadable"))
            .unwrap_err();
        let e = crate::error::line(&e);
        assert!(
            e.contains("Detached from dev without its newest entries"),
            "{e}"
        );
        assert!(store.attached("claude", "s-1").unwrap().is_none());
    }

    #[test]
    fn a_failed_catch_up_still_carries() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["dev"]);
        let store = open(dir.path());
        let transcript = dir.path().join("s-1.jsonl");
        fs::write(&transcript, plain()).unwrap();
        store
            .attach("dev", "claude", "s-1", Some(&transcript))
            .unwrap();
        let tip = |s: &str| {
            let snap = store.thread("dev").unwrap().snapshot().unwrap();
            snap.binding("claude", s).unwrap().map(|b| (b.graft, b.tip))
        };
        let e = store
            .carry_with("claude", "s-1", "s-2", None, |_, _, _| bail!("unreadable"))
            .unwrap_err();
        let e = crate::error::line(&e);
        assert!(
            e.contains("Attached to dev without the newest entries of session s-1: unreadable"),
            "{e}"
        );
        let from = tip("s-1").unwrap().1;
        assert!(from.is_some());
        assert_eq!(tip("s-2"), Some((from, None)));
        assert_eq!(store.carry("claude", "s-9", "s-3", None).unwrap(), None);
        assert_eq!(tip("s-3"), None);
    }

    /// Starts an attach of `s-1` to `dev` and `other`, a rename or removal of `dev`, so both wait
    /// for `dev`'s writer's lock, then lets them go; returns the attach's outcome.
    fn attach_beside(dir: &Path, other: impl FnOnce(Store) + Send + 'static) -> Result<Attached> {
        let store = open(dir);
        let transcript = dir.join("s-1.jsonl");
        fs::write(&transcript, plain()).unwrap();
        let writing = store.thread("dev").unwrap().writer().unwrap();
        let (read_done, observed) = mpsc::channel();
        let path = dir.to_path_buf();
        let attaching = thread::spawn(move || {
            open(&path).attach_with("dev", "claude", "s-1", Some(&transcript), |h, s, p| {
                let t = read(h, s, p)?;
                read_done.send(()).unwrap();
                Ok(t)
            })
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let path = dir.to_path_buf();
        let moving = thread::spawn(move || other(open(&path)));
        thread::sleep(Duration::from_millis(100));
        drop(writing);
        let attached = attaching.join().unwrap();
        moving.join().unwrap();
        attached
    }

    #[test]
    fn an_attach_beside_a_rename_ends_attached_to_the_renamed_thread_or_writes_nothing() {
        for _ in 0..10 {
            let dir = tempfile::TempDir::new().unwrap();
            init(dir.path(), &["dev"]);
            let attached = attach_beside(dir.path(), |s| {
                s.rename("dev", "reviewer", |_| {}).unwrap();
            });
            let store = open(dir.path());
            let owner = store.owner("claude", "s-1").unwrap();
            let snap = store.thread("reviewer").unwrap().snapshot().unwrap();
            match attached {
                Ok(a) => {
                    assert_eq!(a.thread, "dev");
                    assert_eq!(owner.as_deref(), Some("reviewer"));
                    assert!(snap.len().unwrap() > 0);
                }
                Err(e) => {
                    assert!(e.to_string().contains("renamed or removed"), "{e:#}");
                    assert_eq!(owner, None);
                    assert_eq!(snap.track("claude", "s-1").unwrap(), None);
                    assert_eq!(snap.len().unwrap(), 0);
                }
            }
        }
    }

    #[test]
    fn an_attach_beside_a_removal_is_heard_by_it_or_writes_nothing() {
        for _ in 0..10 {
            let dir = tempfile::TempDir::new().unwrap();
            init(dir.path(), &["dev"]);
            let (heard, hearing) = mpsc::channel();
            let attached = attach_beside(dir.path(), move |s| {
                let removed = s.remove(
                    "dev",
                    |_| {},
                    |sessions| match sessions {
                        [] => Ok(()),
                        _ => bail!("attached"),
                    },
                );
                heard.send(removed.map(|r| r.commit).ok()).unwrap();
            });
            let removed = hearing.recv().unwrap();
            let store = open(dir.path());
            match attached {
                Ok(_) => {
                    assert_eq!(removed, None, "the removal missed the attached session");
                    assert_eq!(
                        store.owner("claude", "s-1").unwrap().as_deref(),
                        Some("dev")
                    );
                }
                Err(e) => {
                    assert!(e.to_string().contains("renamed or removed"), "{e:#}");
                    assert_eq!(removed, Some(None), "the removed thread held something");
                    assert!(!store.thread("dev").unwrap().exists().unwrap());
                }
            }
        }
    }

    #[test]
    fn detach_clears_a_session_attached_to_two_threads() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["one", "two"]);
        let store = open(dir.path());
        let transcript = dir.path().join("s-1.jsonl");
        fs::write(&transcript, plain()).unwrap();
        store
            .attach("one", "claude", "s-1", Some(&transcript))
            .unwrap();
        let two = store.thread("two").unwrap();
        two.put_event("claude", "s-1", State::Attached).unwrap();

        let told = "Session claude s-1 is attached to one and two. Detach it, which detaches it \
                    from each (ai agent detach --harness claude --session s-1), then attach it \
                    to one.";
        assert_eq!(
            store.sync("claude", "s-1", None).unwrap_err().to_string(),
            told
        );
        let e = store.attach("two", "claude", "s-1", None).unwrap_err();
        assert_eq!(e.to_string(), told);

        assert_eq!(store.detach("claude", "s-1").unwrap(), ["one", "two"]);
        assert!(store.attached("claude", "s-1").unwrap().is_none());
        assert_eq!(
            store.sync("claude", "s-1", None).unwrap(),
            Synced::NotAttached
        );
        store.attach("two", "claude", "s-1", None).unwrap();
        let (owner, _) = store.attached("claude", "s-1").unwrap().unwrap();
        assert_eq!(owner.name(), "two");
    }

    #[test]
    fn a_session_keeps_the_configuration_directory_its_transcript_names() {
        let dir = tempfile::TempDir::new().unwrap();
        init(dir.path(), &["dev"]);
        let store = open(dir.path());
        let config = |s: &str| {
            let snap = store.thread("dev").unwrap().snapshot().unwrap();
            snap.binding("claude", s).unwrap().unwrap().config
        };
        let work = dir.path().join("work");
        let project = work.join("projects/-repo");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("s-1.jsonl"), plain()).unwrap();
        store
            .attach("dev", "claude", "s-1", Some(&project.join("s-1.jsonl")))
            .unwrap();
        assert_eq!(config("s-1"), Some(work.clone()));
        store.sync("claude", "s-1", None).unwrap();
        assert_eq!(config("s-1"), Some(work.clone()));

        let loose = dir.path().join("s-2.jsonl");
        fs::write(&loose, plain()).unwrap();
        store.attach("dev", "claude", "s-2", Some(&loose)).unwrap();
        assert_eq!(config("s-2"), None);
        let moved = dir.path().join("other/projects/-repo");
        fs::create_dir_all(&moved).unwrap();
        fs::copy(&loose, moved.join("s-2.jsonl")).unwrap();
        store
            .sync("claude", "s-2", Some(&moved.join("s-2.jsonl")))
            .unwrap();
        assert_eq!(config("s-2"), Some(dir.path().join("other")));
    }
}
