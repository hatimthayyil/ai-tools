use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::agent::{
    claude,
    record::{Binding, State, Track, check_id},
    source,
    store::{Added, Store, Thread, Transcript},
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
    match harness {
        claude::HARNESS => {
            let name = Path::new(
                path.file_name()
                    .ok_or_else(|| anyhow::anyhow!("A transcript needs a file name."))?,
            );
            let items = files
                .get(name)
                .map_or_else(Vec::new, |bytes| claude::parse_bytes(session, bytes));
            let agents = claude::subagents_from(&files, &name.with_extension(""), session);
            Ok(Transcript {
                items,
                agents,
                files,
            })
        }
        _ => bail!("ai agent cannot read {harness} sessions."),
    }
}

/// Ingests the new entries of an attached session; a session attached to no thread of the
/// project at `dir` is [`Synced::NotAttached`].
pub fn sync(dir: &Path, harness: &str, session: &str, transcript: Option<&Path>) -> Result<Synced> {
    match Store::find(dir)? {
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
                graft: None,
                tip: None,
            });
        let path = transcript.map(Path::to_path_buf).or(old.transcript.clone());
        let t = read(harness, session, path.as_deref())?;
        let mut b = Track {
            transcript: path,
            ..old.clone()
        };
        let _w = thread.writer()?;
        let added = thread.snapshot()?.ingest(&thread, &mut b, &t)?;
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
            .map(Path::to_path_buf)
            .or_else(|| bound.as_ref()?.transcript.clone());
        let t = read(harness, session, path.as_deref())?;
        let w = target.writer()?;
        let snap = target.snapshot()?;
        let mut b = match bound.clone() {
            Some(b) => Track {
                transcript: path.or(b.transcript),
                ..b
            },
            None => Track {
                harness: harness.into(),
                session: session.into(),
                transcript: path,
                graft: snap.graft()?,
                tip: None,
            },
        };
        let added = snap.ingest(&target, &mut b, &t)?;
        if bound.as_ref() != Some(&b) {
            target.put_track(&b)?;
        }
        drop(w);
        if owner.is_none() {
            self.record(
                harness,
                session,
                State::Attached {
                    thread: name.into(),
                },
            )?;
        }
        Ok(Attached {
            thread: name.into(),
            added,
        })
    }

    /// Detaches a session from its thread after ingesting its new entries, so nothing written
    /// since the last sync is left out; returns the thread, none if it was not attached. A
    /// failed catch-up still detaches, then reports the failure.
    pub fn detach(&self, harness: &str, session: &str) -> Result<Option<String>> {
        self.detach_with(harness, session, read)
    }

    fn detach_with(
        &self,
        harness: &str,
        session: &str,
        read: impl FnOnce(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Option<String>> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let _lock = self.session_lock(harness, session)?;
        let Some(name) = self.owner(harness, session)? else {
            return Ok(None);
        };
        let caught = self.catch_up(&name, harness, session, None, read);
        self.record(harness, session, State::Detached)?;
        caught.with_context(|| format!("Detached from {name} without its newest entries"))?;
        Ok(Some(name))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, sync::mpsc, thread, time::Duration};

    use super::*;

    /// A project at `dir` with `agents`.
    fn init(dir: &Path, agents: &[&str]) {
        crate::project::Project::init(dir).unwrap();
        for a in agents {
            open(dir).create(a).unwrap();
        }
    }

    /// The agent store of the project at `dir`.
    fn open(dir: &Path) -> Store {
        Store::find(dir).unwrap().unwrap()
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
        assert_eq!(
            store.detach("claude", "s-1").unwrap().as_deref(),
            Some(owner.name())
        );
        assert!(store.attached("claude", "s-1").unwrap().is_none());
        assert_eq!(store.detach("claude", "s-1").unwrap(), None);
        let b = owner.snapshot().unwrap().binding("claude", "s-1").unwrap();
        assert!(b.is_some_and(|b| !b.is_attached()));
    }

    #[test]
    fn archived_source_and_projection_use_the_same_captured_bytes() {
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
        let archived = store.dir().join("threads/dev/sources/claude/s-1/s-1.jsonl");
        assert_eq!(
            fs::read(archived).unwrap(),
            original.as_bytes(),
            "archived bytes must match the parsed observation"
        );
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
        assert_eq!(
            store.detach("claude", "s-1").unwrap().as_deref(),
            Some("dev")
        );
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
        let e = format!("{e:#}");
        assert!(
            e.contains("Detached from dev without its newest entries"),
            "{e}"
        );
        assert!(store.attached("claude", "s-1").unwrap().is_none());
    }
}
