use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use jiff::Timestamp;

use crate::{
    chat::{
        claude,
        record::{Binding, check_id},
        source,
        store::{Added, Chat, Thread, Transcript, put_binding},
    },
    git::Changes,
};

/// The outcome of [`sync`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Synced {
    /// The session is attached to no thread of the repository, or there is no repository.
    NotAttached,
    Attached {
        thread: String,
        added: Added,
    },
}

/// The outcome of [`Chat::attach`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attached {
    pub thread: String,
    /// The thread is new.
    pub created: bool,
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
        _ => bail!("ai chat cannot read {harness} sessions."),
    }
}

/// Ingests the new entries of an attached session, in one commit; a session attached to no
/// thread of the repository at `dir` is [`Synced::NotAttached`].
pub fn sync(dir: &Path, harness: &str, session: &str, transcript: Option<&Path>) -> Result<Synced> {
    match Chat::find(dir)? {
        Some(chat) => chat.sync(harness, session, transcript),
        None => Ok(Synced::NotAttached),
    }
}

impl Chat {
    /// The thread `session` is attached to, with its binding there.
    pub fn attached(&self, harness: &str, session: &str) -> Result<Option<(Thread, Binding)>> {
        let _lock = self.session_lock(harness, session)?;
        self.attached_unlocked(harness, session)
    }

    fn attached_unlocked(&self, harness: &str, session: &str) -> Result<Option<(Thread, Binding)>> {
        let mut found: Option<(Thread, Binding)> = None;
        for t in self.threads()? {
            let b = t.snapshot()?.binding(harness, session)?;
            if let Some(b) = b.filter(Binding::is_attached) {
                if found.is_some() {
                    bail!("Session {harness} {session} is attached to multiple threads.");
                }
                found = Some((t, b));
            }
        }
        Ok(found)
    }

    /// Ingests the new entries of an attached session, from `transcript` or the path its
    /// binding holds, in one commit; none if nothing is new.
    pub fn sync(&self, harness: &str, session: &str, transcript: Option<&Path>) -> Result<Synced> {
        self.sync_with(harness, session, transcript, read)
    }

    fn sync_with(
        &self,
        harness: &str,
        session: &str,
        transcript: Option<&Path>,
        mut read: impl FnMut(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Synced> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let Some((thread, _)) = self.attached(harness, session)? else {
            return Ok(Synced::NotAttached);
        };
        let msg = format!("sync {harness} {session}");
        let (added, _) = thread.mutate(&msg, |s| {
            let Some(old) = s.binding(harness, session)?.filter(Binding::is_attached) else {
                return Ok((None, Changes::default()));
            };
            let path = transcript.map(Path::to_path_buf).or(old.transcript.clone());
            let t = read(harness, session, path.as_deref())?;
            let mut b = Binding {
                transcript: path.clone().or(old.transcript.clone()),
                ..old.clone()
            };
            let mut ch = Changes::default();
            let added = s.ingest(&mut b, &t, &mut ch)?;
            if b != old {
                put_binding(&mut ch, &b)?;
            }
            Ok((Some(added), ch))
        })?;
        Ok(match added {
            Some(added) => Synced::Attached {
                thread: thread.name().into(),
                added,
            },
            None => Synced::NotAttached,
        })
    }

    /// Attaches a session to the thread `name`, creating it if new, and ingests its whole
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
        mut read: impl FnMut(&str, &str, Option<&Path>) -> Result<Transcript>,
    ) -> Result<Attached> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let target = self.thread(name)?;
        let _lock = self.session_lock(harness, session)?;
        if let Some((current, _)) = self.attached_unlocked(harness, session)?
            && current.name() != name
        {
            bail!(
                "Session {harness} {session} is attached to {}. Run ai chat detach --harness {harness} --session {session} before attaching to {name}.",
                current.name()
            );
        }
        let created = !target.exists()?;
        let msg = format!("attach {harness} {session}");
        let (added, _) = target.mutate(&msg, |s| {
            let bound = s.binding(harness, session)?;
            let path: Option<PathBuf> = transcript
                .map(Path::to_path_buf)
                .or_else(|| bound.as_ref()?.transcript.clone());
            let t = read(harness, session, path.as_deref())?;
            let now = Timestamp::now();
            let mut b = match bound.as_ref() {
                Some(b) => Binding {
                    transcript: path.clone().or(b.transcript.clone()),
                    attached: if b.is_attached() { b.attached } else { now },
                    detached: None,
                    ..b.clone()
                },
                None => Binding {
                    harness: harness.into(),
                    session: session.into(),
                    transcript: path.clone(),
                    graft: s.len()?.checked_sub(1),
                    tip: None,
                    attached: now,
                    detached: None,
                },
            };
            let mut ch = Changes::default();
            let added = s.ingest(&mut b, &t, &mut ch)?;
            if bound.as_ref() != Some(&b) {
                put_binding(&mut ch, &b)?;
            }
            Ok((added, ch))
        })?;
        Ok(Attached {
            thread: name.into(),
            created,
            added,
        })
    }

    /// Detaches a session from its thread; returns the thread, none if it was not attached.
    pub fn detach(&self, harness: &str, session: &str) -> Result<Option<String>> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let _lock = self.session_lock(harness, session)?;
        let Some((t, _)) = self.attached_unlocked(harness, session)? else {
            return Ok(None);
        };
        detach(&t, harness, session)?;
        Ok(Some(t.name().into()))
    }
}

fn detach(t: &Thread, harness: &str, session: &str) -> Result<()> {
    t.mutate(&format!("detach {harness} {session}"), |s| {
        let mut ch = Changes::default();
        if let Some(b) = s.binding(harness, session)?.filter(Binding::is_attached) {
            let b = Binding {
                detached: Some(Timestamp::now()),
                ..b
            };
            put_binding(&mut ch, &b)?;
        }
        Ok(((), ch))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn an_older_sync_finishing_last_preserves_newer_progress() {
        let dir = tempfile::TempDir::new().unwrap();
        gix::init(dir.path()).unwrap();
        let chat = Chat::find(dir.path()).unwrap().unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/plain.jsonl");
        let all = fs::read_to_string(fixture).unwrap();
        let prefix: String = all.split_inclusive('\n').take(3).collect();
        let transcript = dir.path().join("s-1.jsonl");
        fs::write(&transcript, &prefix).unwrap();
        chat.attach("dev", "claude", "s-1", Some(&transcript))
            .unwrap();
        let (captured, observed) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let path = dir.path().to_path_buf();
        let older = thread::spawn(move || {
            let chat = Chat::find(&path).unwrap().unwrap();
            let mut first = true;
            chat.sync_with("claude", "s-1", None, |harness, session, path| {
                let transcript = read(harness, session, path)?;
                if first {
                    first = false;
                    captured.send(()).unwrap();
                    resumed.recv_timeout(Duration::from_secs(5)).unwrap();
                }
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
        chat.sync("claude", "s-1", None).unwrap();
        let thread = chat.thread("dev").unwrap();
        let before = thread.snapshot().unwrap();
        let items = before.items().unwrap();
        let tip = before.tip("claude", "s-1").unwrap();
        let commit = before.commit();
        resume.send(()).unwrap();
        older.join().unwrap();

        let after = thread.snapshot().unwrap();
        assert_eq!(after.items().unwrap(), items);
        assert_eq!(after.tip("claude", "s-1").unwrap(), tip);
        assert_eq!(after.commit(), commit, "a stale no-op sync adds no commit");
    }

    #[test]
    fn concurrent_attachments_have_one_owner_and_detach_clears_it() {
        let dir = tempfile::TempDir::new().unwrap();
        gix::init(dir.path()).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for name in ["one", "two"] {
            let path = dir.path().to_path_buf();
            let start = start.clone();
            workers.push(thread::spawn(move || {
                let chat = Chat::find(&path).unwrap().unwrap();
                start.wait();
                chat.attach(name, "claude", "s-1", None)
            }));
        }
        start.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let error = results.into_iter().find_map(Result::err).unwrap();
        assert!(error.to_string().contains("Run ai chat detach"));

        let chat = Chat::find(dir.path()).unwrap().unwrap();
        let owners: Vec<_> = chat
            .threads()
            .unwrap()
            .into_iter()
            .filter(|thread| {
                thread
                    .snapshot()
                    .unwrap()
                    .binding("claude", "s-1")
                    .unwrap()
                    .is_some_and(|binding| binding.is_attached())
            })
            .map(|thread| thread.name().to_string())
            .collect();
        assert_eq!(owners.len(), 1, "active attachments: {owners:?}");
        assert_eq!(
            chat.threads().unwrap().len(),
            1,
            "rejected attachment creates no thread"
        );
        assert_eq!(
            chat.detach("claude", "s-1").unwrap(),
            Some(owners[0].clone())
        );
        assert!(chat.attached("claude", "s-1").unwrap().is_none());
        assert_eq!(chat.detach("claude", "s-1").unwrap(), None);
    }

    #[test]
    fn archived_source_and_projection_use_the_same_captured_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        gix::init(dir.path()).unwrap();
        let chat = Chat::find(dir.path()).unwrap().unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/plain.jsonl");
        let all = fs::read_to_string(fixture).unwrap();
        let original: String = all.split_inclusive('\n').take(3).collect();
        let path = dir.path().join("s-1.jsonl");
        fs::write(&path, &original).unwrap();

        chat.attach_with(
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

        let thread = chat.thread("dev").unwrap();
        let snapshot = thread.snapshot().unwrap();
        assert_eq!(snapshot.len().unwrap(), 1);
        assert!(
            &*snapshot
                .as_ref()
                .read("sources/claude/s-1/s-1.jsonl")
                .unwrap()
                == original.as_bytes(),
            "archived bytes must match the parsed observation"
        );
        assert_ne!(fs::read(path).unwrap(), original.as_bytes());
    }
}
