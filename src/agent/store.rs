use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    ops::Range,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{self, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use serde::Serialize;

use crate::{
    agent::{
        claude::Subagent,
        model::{Item, Kind, SourceId, Tree},
        record::{
            Agent, Binding, Event, Head, Key, Line, LineKind, LineOp, Pointer, Row, State, Tag,
            Track, check_id,
        },
        source::{self, Version},
    },
    backup,
    config::{Homes, Settings},
    error,
    file::{self, AtPath, human, pretty},
    jsonl,
    memory::{self, CALLS, Who},
    project::Project,
    repo,
};

/// The agent store format this ai reads and writes.
pub const FORMAT: u32 = 6;
/// The agent store's directory in a project's folder of `~/ai`, and of this machine's files for
/// the project in `~/.ai`.
pub const DIR: &str = "agents";
/// The store's directory of threads, one directory each.
pub const THREADS: &str = "threads";
/// A thread's items.
pub const ITEMS: &str = "items.jsonl";
/// A thread's summary lines.
pub const LINES: &str = "lines.jsonl";
/// A thread's session [tracks](super::record::Track).
pub const SESSIONS: &str = "sessions.jsonl";
/// A thread's attachment [changes](super::record::Event).
pub const ATTACHMENTS: &str = "attachments.jsonl";
/// A thread's subagents.
pub const AGENTS: &str = "agents.jsonl";
/// What a thread read of its sessions' files: [records](super::record::Pointer).
pub const SOURCES: &str = "sources.jsonl";
/// The lock every writer of a thread's files takes.
const WRITE_LOCK: &str = "write.lock";
/// The compactor's lock on a thread, and its log.
const COMPACT_LOCK: &str = "compact.lock";
const COMPACT_LOG: &str = "compact.log";
/// In `~/ai`'s git directory: what a committer [`Store::spawn_committer`] started printed.
const COMMITTER_LOG: &str = "ai-committer.log";

/// `name` if it names a thread: letters, digits, `-` and `_`, starting with a letter or digit.
pub fn check_name(name: &str) -> Result<&str> {
    let ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        bail!("'{name}' is not a thread name: letters, digits, '-' and '_'.");
    }
    Ok(name)
}

/// Makes every commit `~/ai`'s queue holds ([`repo::commit_asked`]), waiting for a commit under
/// way, then starts `git gc` if `settings` say it is due ([`gc`]); how many asks it committed.
pub fn commit_asked(homes: &Homes, settings: &Settings) -> Result<usize> {
    let n = repo::commit_asked(&homes.repo, "ai")?;
    gc(homes, settings);
    Ok(n)
}

/// After a commit to `~/ai`, starts `git gc` on it once the loose objects written since the last
/// one pass `[agent] gc-after-mib` as `settings` read it. The commit stands whatever happens here,
/// so a problem is only told: a bad `[agent]` table each time, a missing `git` once per process.
pub(super) fn gc(homes: &Homes, settings: &Settings) {
    static NO_GIT: AtomicBool = AtomicBool::new(false);
    let limit = match settings.agent_table() {
        Ok(t) => t.gc_after(),
        Err(e) => {
            eprintln!("Cannot pack {}: {}", pretty(&homes.repo), error::line(&e));
            return;
        }
    };
    match repo::gc(&homes.repo, limit) {
        Ok(repo::Gc::Within | repo::Gc::Started) => {}
        Ok(repo::Gc::NoGit) => {
            if !NO_GIT.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "Cannot pack {}: no git program on PATH. Install git: ai runs git gc on it \
                     once more than [agent] gc-after-mib of loose objects have been written \
                     since its last one.",
                    pretty(&homes.repo)
                );
            }
        }
        Err(e) => eprintln!("Cannot pack {}: {}", pretty(&homes.repo), error::line(&e)),
    }
}

/// The items nothing follows, in order.
fn tips(heads: &[Head]) -> Vec<u64> {
    let mut tip = vec![true; heads.len()];
    for p in heads.iter().filter_map(|h| h.parent) {
        tip[p as usize] = false;
    }
    (0..heads.len() as u64)
        .filter(|&i| tip[i as usize])
        .collect()
}

/// The turns on the branch to `tip`, as `model::turns` counts them: one, plus one per item
/// below the root that opens a turn.
fn count_turns(heads: &[Head], opens: &[bool], tip: u64) -> usize {
    let mut n = 1;
    let mut at = tip as usize;
    while let Some(p) = heads[at].parent {
        n += usize::from(opens[at]);
        at = p as usize;
    }
    n
}

/// The dedupe key of an entry: its harness and entry id.
fn key(id: &SourceId) -> (String, String) {
    (id.harness.clone(), id.entry.clone())
}

/// What a sync or attach added to a thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Added {
    /// The new items of the main tree.
    pub items: Range<u64>,
    /// How many subagent items are new.
    pub agent_items: u64,
    /// A warning for each file of the session that no longer holds what the thread read of it:
    /// one that is gone, on every sync; one that no longer begins with it, on the sync that
    /// finds so.
    pub unheld: Vec<String>,
}

impl Added {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.agent_items == 0
    }
}

/// A session as its harness's adapter reads it: its items, its subagents, and its files by
/// their path in `dir`, the transcript's directory; `main` is the transcript's own.
#[derive(Debug, Default)]
pub struct Transcript {
    pub items: Vec<Item>,
    pub agents: Vec<Subagent>,
    pub files: BTreeMap<PathBuf, Vec<u8>>,
    pub dir: PathBuf,
    pub main: PathBuf,
}

/// A thread at a glance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadInfo {
    pub name: String,
    /// Its newest item: its harness, session and time.
    pub newest: Option<Head>,
    pub tips: Vec<u64>,
    /// The turns on its newest branch.
    pub turns: usize,
    /// Its sessions, each with the time of its newest item (else of its attaching), newest
    /// first.
    pub sessions: Vec<(Binding, Timestamp)>,
    /// Every version of its sessions' files that it read; the error if its source records
    /// can't be read, which leaves the rest of the thread listed.
    pub sources: Result<Vec<Version>, String>,
}

/// A state of a thread: the lengths of its item, line and session files then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    items: u64,
    lines: u64,
    sessions: u64,
}

/// The agents of one project, shared by its checkouts: a directory of [threads](Thread) in the
/// project's [folder](Project::folder) of the user's repository `~/ai`, which `ai` commits;
/// beside them the store's format. The threads' locks and logs and the sessions' locks are this
/// machine's, in `~/.ai`.
pub struct Store {
    project: Project,
    homes: Homes,
    root: PathBuf,
    /// This machine's files for the project's agents.
    local: PathBuf,
    cwd: PathBuf,
}

impl Store {
    /// The agents of the project at or above `dir`, in the user's `~/ai`; `None` outside a
    /// project. A store of another format, or agents ai 0.17 kept and [`Project::unconverted`] finds, are
    /// refused.
    pub fn find(dir: &Path) -> Result<Option<Store>> {
        match Project::find(dir)? {
            Some(p) => Ok(Some(Store::of(p, &Homes::user()?, dir)?)),
            None => Ok(None),
        }
    }

    /// [`Store::find`] in `homes`.
    pub fn find_in(homes: &Homes, dir: &Path) -> Result<Option<Store>> {
        Project::find(dir)?
            .map(|p| Store::of(p, homes, dir))
            .transpose()
    }

    /// [`Store::find`], else an error saying how to make the project.
    pub fn open(dir: &Path) -> Result<Store> {
        Store::open_in(&Homes::user()?, dir)
    }

    /// [`Store::open`] in `homes`.
    pub fn open_in(homes: &Homes, dir: &Path) -> Result<Store> {
        Store::of(Project::require(dir)?, homes, dir)
    }

    /// Where the agents of `project` live: `agents` in its [folder](Project::folder) of
    /// `homes.repo`.
    pub fn root(homes: &Homes, project: &Project) -> Result<PathBuf> {
        Ok(project.folder(homes)?.join(DIR))
    }

    fn of(project: Project, homes: &Homes, cwd: &Path) -> Result<Store> {
        project.refuse_unconverted(homes)?;
        let root = Store::root(homes, &project)?;
        file::check_format(&root, FORMAT, "agent")?;
        Ok(Store {
            local: homes.machine.join(DIR).join(&project.id),
            project,
            homes: homes.clone(),
            root,
            cwd: cwd.to_path_buf(),
        })
    }

    pub(super) fn project(&self) -> &Project {
        &self.project
    }

    /// Where it lives: the user's `~/ai` and this machine's `~/.ai`.
    pub fn homes(&self) -> &Homes {
        &self.homes
    }

    /// The directory it was found from.
    pub(super) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.root
    }

    /// Claims the project's folder and makes the store's directory and format file, before its
    /// first write.
    fn ensure(&self) -> Result<()> {
        self.project.claim(&self.homes)?;
        file::ensure_format(&self.root, FORMAT)
    }

    pub(super) fn session_lock(&self, harness: &str, session: &str) -> Result<file::Lock> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let dir = self.local.join("sessions").join(harness);
        fs::create_dir_all(&dir).at(&dir)?;
        file::lock(&dir.join(format!("{session}.lock")))
    }

    /// The thread `session` is attached to: the one whose last change of the session's
    /// attachment attached it. An error if several did, which no command of ai records but a
    /// merge of `~/ai` can leave; [`Store::detach`] clears it.
    pub(super) fn owner(&self, harness: &str, session: &str) -> Result<Option<String>> {
        let mut owners = self.owners(harness, session)?;
        match owners.len() {
            0 | 1 => Ok(owners.pop()),
            _ => bail!(
                "Session {harness} {session} is attached to {}. Detach it, which detaches it \
                 from each (ai agent detach --harness {harness} --session {session}), then \
                 attach it to one.",
                owners.join(" and ")
            ),
        }
    }

    /// Every thread `session` is attached to, by name. The caller holds the session's lock,
    /// which a rename of its thread holds too, so a thread renamed away while this reads it
    /// holds no attachment of the session and is passed over.
    pub(super) fn owners(&self, harness: &str, session: &str) -> Result<Vec<String>> {
        let mut owners = Vec::new();
        for t in self.threads()? {
            let events = match t.events() {
                Ok(e) => e,
                Err(_) if !t.exists()? => continue,
                Err(e) => return Err(e),
            };
            let last = events
                .into_iter()
                .rfind(|e| e.harness == harness && e.session == session);
            if last.is_some_and(|e| e.state == State::Attached) {
                owners.push(t.name);
            }
        }
        Ok(owners)
    }

    /// The thread of agent `name`; it exists once [`Store::create`] makes it.
    pub fn thread(&self, name: &str) -> Result<Thread> {
        check_name(name)?;
        Ok(Thread {
            name: name.into(),
            dir: self.root.join(THREADS).join(name),
            local: self.local.join(THREADS).join(name),
        })
    }

    /// Agent `name`'s thread; an error if there is no such agent.
    pub fn existing(&self, name: &str) -> Result<Thread> {
        let t = self.thread(name)?;
        if !t.exists()? {
            bail!("No agent {name} in this project. Create it: ai agent new {name}");
        }
        Ok(t)
    }

    /// Agent `name`'s address, `<project>.<name>`: the name of the project's folder in `~/ai`,
    /// which every checkout and clone shares, a dot, then the agent's name. An agent's name holds
    /// no dot, so an address splits at its last dot. An error if the folder's name holds a
    /// character an address can't.
    pub fn address(&self, name: &str) -> Result<String> {
        check_name(name)?;
        let folder = self.root.parent().unwrap_or(&self.root);
        let project = folder
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let ok = !project.is_empty()
            && project
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !ok {
            bail!(
                "The project's folder {} has a name an agent's address can't begin with: use \
                 letters, digits, '.', '-' and '_'. Rename the folder; ai finds it by its id.",
                pretty(folder)
            );
        }
        Ok(format!("{project}.{name}"))
    }

    /// Creates agent `name` with an empty thread; an error if it exists. A summariser's log
    /// left on this machine under the name, a former agent's, first moves aside, so the new
    /// agent starts a log of its own.
    pub fn create(&self, name: &str) -> Result<Thread> {
        let t = self.thread(name)?;
        self.ensure()?;
        let threads = self.root.join(THREADS);
        fs::create_dir_all(&threads).at(&threads)?;
        if !t.exists()? {
            t.retire_log()?;
        }
        match fs::create_dir(&t.dir) {
            Ok(()) => Ok(t),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                bail!("Agent {name} already exists in this project.")
            }
            Err(e) => Err(e).at(&t.dir),
        }
    }

    /// Stills thread `t`, and `beside` with it: takes their compactor locks, then the locks of
    /// the sessions attached to `t`, then their writer locks, each kind in name order. While
    /// they are held, no summariser, sync or attach of `t` is mid-write, and one that found `t`
    /// before fails at its next lock if `t` has moved. `waiting` hears the name of each
    /// compactor it waits for. An attach records its attachment under the writer's lock, so a
    /// session attached while this waited for that lock is seen, and its lock taken too.
    fn still(&self, t: &Thread, beside: Option<&Thread>, waiting: impl Fn(&str)) -> Result<Still> {
        let mut threads: Vec<&Thread> = std::iter::once(t).chain(beside).collect();
        threads.sort_by(|a, b| a.name.cmp(&b.name));
        let mut held = Vec::new();
        for t in &threads {
            let p = t.local_file(COMPACT_LOCK)?;
            held.push(match file::try_lock(&p)? {
                Some(l) => l,
                None => {
                    waiting(&t.name);
                    file::lock(&p)?
                }
            });
        }
        loop {
            let attached = t.attached()?;
            let mut locks = attached
                .iter()
                .map(|(h, s)| self.session_lock(h, s))
                .collect::<Result<Vec<_>>>()?;
            for t in &threads {
                locks.push(file::lock(&t.local_file(WRITE_LOCK)?)?);
            }
            t.here()?;
            if t.attached()?.iter().all(|s| attached.contains(s)) {
                held.extend(locks);
                return Ok(Still {
                    attached,
                    _locks: held,
                });
            }
        }
    }

    /// Renames agent `old` to `new`, a name no agent has: its thread moves to `threads/<new>`,
    /// and its sessions stay attached, since a thread holds its sessions' attachments.
    /// `waiting` hears the name of each compactor it waits for.
    ///
    /// Both threads are [still](Store::still) meanwhile. The move is one rename that never
    /// replaces, under `~/ai`'s commit lock, so a crash leaves the thread whole under one name.
    /// This machine's compactor log then follows it; the agent is renamed whether or not the
    /// log could move, which a warning tells. `old`'s lock files stay where they are: a process
    /// may still wait on one, and a new file in its place would let a second process hold it.
    pub fn rename(&self, old: &str, new: &str, waiting: impl Fn(&str)) -> Result<Thread> {
        let from = self.existing(old)?;
        let to = self.thread(new)?;
        if old == new {
            bail!("Agent {old} is already called {new}.");
        }
        if to.exists()? {
            bail!("Agent {new} already exists in this project.");
        }
        let _still = self.still(&from, Some(&to), waiting)?;
        {
            let _commits = repo::lock(&self.homes.repo)?;
            match file::rename_new(&from.dir, &to.dir) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    bail!("Agent {new} already exists in this project.")
                }
                r => r.at(&from.dir)?,
            }
        }
        if let Err(e) = from.move_log(&to) {
            eprintln!(
                "Agent {old} is now {new}, but its summariser's log stayed at {}: {}",
                pretty(&from.log_path()),
                error::line(&e)
            );
        }
        Ok(to)
    }

    /// Removes agent `name`: its thread leaves the store and `~/ai`'s history keeps it.
    /// `waiting` hears the name of the compactor it waits for; `refuse` gets the sessions
    /// attached to the thread, by harness and session id, and stops the removal with its error.
    ///
    /// The thread is [still](Store::still) meanwhile. The project's folder is committed first:
    /// that commit holds every turn the thread has. A file under the thread that git leaves out
    /// stops it before. The thread then moves into `~/ai`'s git directory in one rename, under
    /// the commit lock, so no lookup and no commit sees it half gone; [`Removed::delete`]
    /// deletes it there. This machine's compactor log moves aside last, so an agent made later
    /// under the name starts a log of its own; the agent is removed whether or not the log
    /// could move, which a warning tells. The lock files stay, as a rename leaves them.
    pub fn remove(
        &self,
        name: &str,
        waiting: impl Fn(&str),
        refuse: impl Fn(&[(String, String)]) -> Result<()>,
    ) -> Result<Removed> {
        let t = self.existing(name)?;
        let still = self.still(&t, None, waiting)?;
        refuse(&still.attached)?;
        let path = self.path_in_repo(&t);
        if let Some(f) = repo::left_out(&self.homes.repo, &path)?.first() {
            bail!(
                "Cannot remove agent {name}: git leaves out {}, so ~/ai's history could not \
                 keep it. Move it out of the thread, then try again.",
                pretty(f)
            );
        }
        self.commit(&format!("ai agent remove {name}: its last state"))?;
        let _commits = repo::lock(&self.homes.repo)?;
        let commit = repo::head_holding(&self.homes.repo, &path)?;
        let copy = tempfile::Builder::new()
            .prefix("ai-remove-")
            .tempdir_in(repo::git_dir(&self.homes.repo))
            .context("Cannot make a directory to remove the thread in.")?;
        file::rename_new(&t.dir, &copy.path().join(name)).at(&t.dir)?;
        if let Err(e) = t.retire_log() {
            eprintln!(
                "Agent {name} is removed, but its summariser's log stayed at {}: {}",
                pretty(&t.log_path()),
                error::line(&e)
            );
        }
        Ok(Removed {
            commit: commit.map(|c| c.to_string()),
            path,
            copy,
        })
    }

    /// Thread `t`'s path in `~/ai`.
    fn path_in_repo(&self, t: &Thread) -> PathBuf {
        t.dir
            .strip_prefix(&self.homes.repo)
            .expect("a thread is in the user's repository")
            .to_path_buf()
    }

    /// Restores agent `name` as commit `commit` of `~/ai` holds its thread, the id
    /// [`Store::remove`] gave; an error if an agent has the name, so one made since is never
    /// replaced. A session the thread held attached that another thread holds attached now is
    /// detached from the restored one, and named in [`Restored::detached`].
    ///
    /// The thread is written in `~/ai`'s git directory, then moved into place in one rename
    /// that never replaces, under the commit lock, with the locks of its attached sessions held.
    pub fn restore(&self, name: &str, commit: &str) -> Result<Restored> {
        let t = self.thread(name)?;
        let taken = || {
            anyhow!(
                "Agent {name} exists in this project, and restoring would replace it. Rename it \
                 first: ai agent rename {name} <new name>"
            )
        };
        if t.exists()? {
            return Err(taken());
        }
        self.ensure()?;
        let threads = self.root.join(THREADS);
        fs::create_dir_all(&threads).at(&threads)?;
        let copy = tempfile::Builder::new()
            .prefix("ai-restore-")
            .tempdir_in(repo::git_dir(&self.homes.repo))
            .context("Cannot make a directory to restore the thread in.")?;
        let staged = Thread {
            name: name.into(),
            dir: copy.path().join(name),
            local: t.local.clone(),
        };
        if !repo::checkout(
            &self.homes.repo,
            commit,
            &self.path_in_repo(&t),
            &staged.dir,
        )? {
            bail!(
                "Commit {commit} of {} holds no agent {name} of this project.",
                pretty(&self.homes.repo)
            );
        }
        let attached = staged.attached()?;
        let _sessions = attached
            .iter()
            .map(|(h, s)| self.session_lock(h, s))
            .collect::<Result<Vec<_>>>()?;
        let mut detached = Vec::new();
        for (harness, session) in attached {
            let owners = self.owners(&harness, &session)?;
            if let Some(owner) = owners.into_iter().next() {
                staged.put_event(&harness, &session, State::Detached)?;
                detached.push((harness, session, owner));
            }
        }
        t.retire_log()?;
        let _commits = repo::lock(&self.homes.repo)?;
        match file::rename_new(&staged.dir, &t.dir) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(taken()),
            r => r.at(&t.dir)?,
        }
        Ok(Restored { detached })
    }

    /// Commits the project's folder of `~/ai` as `msg`, with whatever else the queue holds
    /// ([`Store::commit_asked`]), leaving the rest of the repository as it is; nothing if
    /// nothing changed. It waits for a commit under way.
    pub fn commit(&self, msg: &str) -> Result<()> {
        self.ask(msg)?;
        self.commit_asked()?;
        Ok(())
    }

    /// Asks for the project's folder of `~/ai` to be committed as `msg` by a committer of its
    /// own, started unless one is running ([`Store::spawn_committer`]), and returns without
    /// waiting for it. Claude Code ends a hook that outlasts its bound, SIGKILL included, so a
    /// commit made in a hook's process can be cut short with git's lock left behind; the
    /// committer is outside the hook's processes once the hook exits.
    pub fn commit_later(&self, msg: &str) -> Result<()> {
        if self.ask(msg)? && !repo::committing(&self.homes.repo)? {
            self.spawn_committer()?;
        }
        Ok(())
    }

    /// Queues the project's folder to be committed as `msg`; whether it did, which it does not
    /// when the folder does not exist.
    fn ask(&self, msg: &str) -> Result<bool> {
        let folder = self.project.folder(&self.homes)?;
        if !folder.exists() {
            return Ok(false);
        }
        let rel = folder
            .strip_prefix(&self.homes.repo)
            .expect("a project's folder is in the user's repository");
        repo::ask(&self.homes.repo, rel, msg)?;
        Ok(true)
    }

    /// [`commit_asked`] with what `ai.toml` sets where this store was found.
    pub fn commit_asked(&self) -> Result<usize> {
        commit_asked(&self.homes, &self.settings())
    }

    /// Starts `ai agent commit` in this store's directory, detached: in a session and process
    /// group of its own, with no stdin or stdout and stderr appended to
    /// `~/ai/.git/ai-committer.log`, so it holds none of the caller's pipes and outlives it.
    fn spawn_committer(&self) -> Result<()> {
        let exe = std::env::current_exe().context("Cannot find this ai to start a commit.")?;
        let log = repo::git_dir(&self.homes.repo).join(COMMITTER_LOG);
        let mut c = process::Command::new(exe);
        c.args(["agent", "commit"])
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(memory::log::open(&log)?);
        // SAFETY: setsid is async-signal-safe, and the closure touches no other state.
        unsafe {
            c.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        c.spawn().context("Cannot start a commit of ~/ai.")?;
        Ok(())
    }

    /// Every thread, by name.
    pub fn threads(&self) -> Result<Vec<Thread>> {
        let dir = self.root.join(THREADS);
        let mut names = Vec::new();
        match fs::read_dir(&dir) {
            Ok(entries) => {
                for e in entries {
                    let e = e.at(&dir)?;
                    if let Some(n) = e.file_name().to_str().filter(|n| check_name(n).is_ok()) {
                        names.push(n.to_string());
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).at(&dir),
        }
        names.sort();
        names.iter().map(|n| self.thread(n)).collect()
    }

    /// Every thread with its newest item, tips, turns and sessions; newest first.
    pub fn list(&self) -> Result<Vec<ThreadInfo>> {
        let mut list = self
            .threads()?
            .iter()
            .map(|t| {
                let snap = t.snapshot()?;
                let heads = snap.heads()?;
                let tips = tips(&heads);
                let turns = match tips.last() {
                    Some(&tip) => count_turns(&heads, &snap.opens()?, tip),
                    None => 0,
                };
                let mut sessions: Vec<(Binding, Timestamp)> = snap
                    .bindings()?
                    .into_iter()
                    .map(|b| {
                        let newest = b.tip.and_then(|t| heads.get(t as usize));
                        let at = newest.map_or(b.attached, |h| h.ts);
                        (b, at)
                    })
                    .collect();
                sessions.sort_by_key(|s| std::cmp::Reverse(s.1));
                Ok(ThreadInfo {
                    name: t.name.clone(),
                    tips,
                    newest: heads.into_iter().last(),
                    turns,
                    sessions,
                    sources: snap.versions().map_err(|e| error::line(&e)),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        list.sort_by_key(|t| std::cmp::Reverse(t.newest.as_ref().map(|h| h.ts)));
        Ok(list)
    }

    /// Whether `dir` shares this store: an existing directory of this project, in any checkout or
    /// clone of it.
    pub fn holds(&self, dir: &Path) -> bool {
        Project::find(dir).is_ok_and(|p| p.is_some_and(|p| p.id == self.project.id))
    }
}

/// The changes of attachment in `bytes`, thread `name`'s attachments file.
fn events(name: &str, bytes: &[u8]) -> Result<Vec<Event>> {
    jsonl::decode_all(bytes)
        .with_context(|| format!("Thread {name}: the attachments file is corrupt"))
}

/// What [`Store::still`] holds: the locks that keep a thread still, until dropped.
struct Still {
    /// The sessions attached to the thread, by harness and session id.
    attached: Vec<(String, String)>,
    _locks: Vec<file::Lock>,
}

/// What [`Store::restore`] did beyond putting the thread back.
#[derive(Debug, PartialEq, Eq)]
pub struct Restored {
    /// Each session the restored thread no longer holds attached, by harness and session id,
    /// with the thread that holds it attached now.
    pub detached: Vec<(String, String, String)>,
}

/// A thread [`Store::remove`] took out of the store, waiting in `~/ai`'s git directory to be
/// [deleted](Removed::delete); dropped, it is deleted quietly.
pub struct Removed {
    /// The commit of `~/ai` that holds the thread's last state; `None` when git never recorded
    /// it, as it held no file.
    pub commit: Option<String>,
    /// The thread's path in `~/ai`.
    pub path: PathBuf,
    copy: tempfile::TempDir,
}

impl Removed {
    /// Deletes the thread's files from `~/ai`'s git directory.
    pub fn delete(self) -> Result<()> {
        let at = self.copy.path().to_path_buf();
        self.copy.close().map_err(|e| {
            anyhow!(
                "Cannot delete the removed thread's files in {}: {e}. Delete them yourself.",
                pretty(&at)
            )
        })
    }
}

/// One thread: a directory `threads/<name>` of the store; its locks and its compactor's log
/// are this machine's, in `local`.
pub struct Thread {
    name: String,
    dir: PathBuf,
    local: PathBuf,
}

impl Thread {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn exists(&self) -> Result<bool> {
        Ok(self.dir.is_dir())
    }

    /// The thread as it is now; empty if it does not exist. A record names only items appended
    /// before it, so the records are read first and the items last: the state is whole without
    /// the writer's lock. Files read from a thread renamed away meanwhile may be of two states,
    /// so a snapshot that read anything from a directory gone by its end is an error.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let events = jsonl::read(&self.dir.join(ATTACHMENTS))?;
        let tracks = jsonl::read(&self.dir.join(SESSIONS))?;
        let sources = jsonl::read(&self.dir.join(SOURCES))?;
        let lines = jsonl::read(&self.dir.join(LINES))?;
        let agents = jsonl::read(&self.dir.join(AGENTS))?;
        let s = Snapshot {
            name: self.name.clone(),
            dir: self.dir.clone(),
            items: jsonl::read(&self.dir.join(ITEMS))?,
            lines,
            tracks,
            sources,
            agents,
            events,
        };
        let read = [
            &s.items, &s.lines, &s.tracks, &s.sources, &s.agents, &s.events,
        ];
        if read.iter().any(|b| !b.is_empty()) {
            self.here()?;
        }
        Ok(s)
    }

    /// An error once the thread's directory is gone: renamed or removed since this process
    /// found it.
    fn here(&self) -> Result<()> {
        if !self.dir.is_dir() {
            bail!("Agent {} was renamed or removed while this ran.", self.name);
        }
        Ok(())
    }

    /// `lock`, one of the thread's locks, taken once whoever held it let go; an error if the
    /// thread is gone by then. A rename holds the thread's locks, so a writer that found the
    /// thread before it moved fails here rather than remaking it at its old name.
    fn held(&self, lock: file::Lock) -> Result<file::Lock> {
        self.here()?;
        Ok(lock)
    }

    /// The writers' lock on this thread's files; an error if the thread is gone.
    pub(super) fn writer(&self) -> Result<file::Lock> {
        self.held(file::lock(&self.local_file(WRITE_LOCK)?)?)
    }

    /// The file `name` among this machine's files for the thread, their directory made.
    fn local_file(&self, name: &str) -> Result<PathBuf> {
        fs::create_dir_all(&self.local).at(&self.local)?;
        Ok(self.local.join(name))
    }

    fn append<T: Serialize>(&self, file: &str, rows: &[T]) -> Result<()> {
        jsonl::append(&self.dir.join(file), rows)
    }

    /// The changes of the attachments of sessions to this thread, in order, read without the
    /// rest of the thread; an error if the thread went while this read them.
    fn events(&self) -> Result<Vec<Event>> {
        let bytes = jsonl::read(&self.dir.join(ATTACHMENTS))?;
        if !bytes.is_empty() {
            self.here()?;
        }
        events(&self.name, &bytes)
    }

    /// The sessions attached here, by harness and session id, in order: those whose last
    /// change of attachment to this thread attached them, as [`Store::owners`] finds them.
    fn attached(&self) -> Result<Vec<(String, String)>> {
        let mut last = BTreeMap::new();
        for e in self.events()? {
            last.insert((e.harness, e.session), e.state);
        }
        Ok(last
            .into_iter()
            .filter(|(_, s)| *s == State::Attached)
            .map(|(k, _)| k)
            .collect())
    }

    /// Moves this machine's compactor log to `to`'s. A log already there is a former agent's
    /// of that name, and first moves aside as [`Thread::retire_log`] moves it. The caller holds
    /// both threads' compactor locks.
    fn move_log(&self, to: &Thread) -> Result<()> {
        to.retire_log()?;
        let from = self.log_path();
        match file::rename_new(&from, &to.log_path()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r.at(&from),
        }
    }

    /// Moves this machine's compactor log aside, to `compact.removed-<time>.log` beside it,
    /// numbered `-2`, … after the time when that name is taken; the caller holds the compactor's
    /// lock.
    fn retire_log(&self) -> Result<()> {
        let from = self.log_path();
        let stamp = Timestamp::now().strftime("%Y%m%dT%H%M%S").to_string();
        for n in 1.. {
            let suffix = match n {
                1 => String::new(),
                n => format!("-{n}"),
            };
            let to = self
                .local
                .join(format!("compact.removed-{stamp}{suffix}.log"));
            match file::rename_new(&from, &to) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                r => return r.at(&from),
            }
        }
        unreachable!("a free name comes before the numbers run out")
    }

    /// Records a session's branch; the caller holds the writer's lock.
    pub(super) fn put_track(&self, t: &Track) -> Result<()> {
        self.append(SESSIONS, std::slice::from_ref(t))
    }

    /// Records a change of a session's attachment to this thread; the caller holds the
    /// session's lock.
    pub(super) fn record(&self, harness: &str, session: &str, state: State) -> Result<()> {
        let _w = self.writer()?;
        self.put_event(harness, session, state)
    }

    /// Appends a change of a session's attachment to this thread; the caller holds the
    /// session's lock and the writer's.
    pub(super) fn put_event(&self, harness: &str, session: &str, state: State) -> Result<()> {
        let event = Event {
            harness: harness.into(),
            session: session.into(),
            at: Timestamp::now(),
            state,
        };
        self.append(ATTACHMENTS, &[event])
    }

    /// The compactor's lock on this thread; `None` while another process holds it. An error if
    /// the thread is gone.
    pub fn try_lock(&self) -> Result<Option<file::Lock>> {
        file::try_lock(&self.local_file(COMPACT_LOCK)?)?
            .map(|l| self.held(l))
            .transpose()
    }

    /// The compactor's lock on this thread, waiting for it however long another process holds
    /// it. An error if the thread is gone.
    pub fn lock(&self) -> Result<file::Lock> {
        self.held(file::lock(&self.local_file(COMPACT_LOCK)?)?)
    }

    /// The compactor's lock on this thread, waiting for it until `deadline`; `None` if another
    /// process still holds it then. An error if the thread is gone.
    pub fn lock_before(&self, deadline: Instant) -> Result<Option<file::Lock>> {
        file::lock_before(&self.local_file(COMPACT_LOCK)?, deadline)?
            .map(|l| self.held(l))
            .transpose()
    }

    /// The compactor's log, beside its lock, on this machine.
    pub fn log_path(&self) -> PathBuf {
        self.local.join(COMPACT_LOG)
    }

    /// Where the compactor keeps its calls to the model: `threads/<name>/calls`.
    pub fn calls_dir(&self) -> PathBuf {
        self.dir.join(CALLS)
    }

    /// Drops every line; how many it dropped.
    pub fn drop_lines(&self) -> Result<usize> {
        let _w = self.writer()?;
        let n = self.snapshot()?.all_lines()?.len();
        if n > 0 {
            self.append(LINES, &[LineOp::Clear])?;
        }
        Ok(n)
    }

    /// Drops the lines among `keys` that are built; how many it dropped.
    pub fn drop_keys(&self, keys: &[Key]) -> Result<usize> {
        let _w = self.writer()?;
        let lines = self.snapshot()?.all_lines()?;
        let keys: Vec<Key> = keys
            .iter()
            .copied()
            .filter(|k| lines.contains_key(k))
            .collect();
        if !keys.is_empty() {
            self.append(LINES, &[LineOp::Drop { keys: keys.clone() }])?;
        }
        Ok(keys.len())
    }

    /// Writes the line `key` unless it is built; returns the line stored.
    pub fn put_line(&self, key: Key, kind: LineKind, who: &Who, text: &str) -> Result<Line> {
        let line = Line {
            ts: Timestamp::now(),
            kind,
            who: who.clone(),
            text: text.into(),
        };
        let mut stored = self.put_lines(vec![(key, line)])?;
        Ok(stored.remove(0))
    }

    /// Writes the lines not built yet; returns the line stored under each key.
    pub fn put_lines(&self, lines: Vec<(Key, Line)>) -> Result<Vec<Line>> {
        let _w = self.writer()?;
        let s = self.snapshot()?;
        let n = s.len()?;
        let mut built = s.all_lines()?;
        let mut ops = Vec::new();
        let mut stored = Vec::with_capacity(lines.len());
        for (key, line) in lines {
            if let Some(l) = built.get(&key) {
                stored.push(l.clone());
                continue;
            }
            if key.last >= n {
                bail!("Line {key} is beyond the thread: it holds {n} items.");
            }
            built.insert(key, line.clone());
            stored.push(line.clone());
            ops.push(LineOp::Put { key, line });
        }
        self.append(LINES, &ops)?;
        Ok(stored)
    }
}

/// A thread as read at one moment: every read through it sees the same state.
pub struct Snapshot {
    name: String,
    dir: PathBuf,
    items: Vec<u8>,
    lines: Vec<u8>,
    tracks: Vec<u8>,
    sources: Vec<u8>,
    agents: Vec<u8>,
    events: Vec<u8>,
}

fn check_head(thread: &str, what: &str, i: u64, h: &Head) -> Result<()> {
    if h.seq != i || h.parent.is_some_and(|p| p >= i) {
        bail!(
            "Thread {thread}: {what} {i} is corrupt: it is numbered {} with parent {:?}.",
            h.seq,
            h.parent
        );
    }
    Ok(())
}

/// The records of one item sequence, checked: positions are sequence numbers, parents earlier.
fn decode_seq<T: serde::de::DeserializeOwned>(
    thread: &str,
    what: &str,
    b: &[u8],
    head: impl Fn(&T) -> &Head,
) -> Result<Vec<T>> {
    jsonl::lines(b)
        .enumerate()
        .map(|(i, l)| {
            let r: T = jsonl::decode(l)
                .with_context(|| format!("Thread {thread}: {what} {i} is corrupt"))?;
            check_head(thread, what, i as u64, head(&r))?;
            Ok(r)
        })
        .collect()
}

fn heads(thread: &str, what: &str, b: &[u8]) -> Result<Vec<Head>> {
    decode_seq(thread, what, b, |h| h)
}

/// Items from the rows of one sequence: each parent's source id from its row.
fn items(thread: &str, what: &str, b: &[u8]) -> Result<Vec<Item>> {
    let rows: Vec<Row> = decode_seq(thread, what, b, |r: &Row| &r.head)?;
    let ids: Vec<SourceId> = rows.iter().map(|r| r.head.source()).collect();
    Ok(rows
        .into_iter()
        .map(|r| Item {
            id: r.head.source(),
            parent: r.head.parent.map(|p| ids[p as usize].clone()),
            ts: r.head.ts,
            kind: r.kind,
            ends_turn: r.ends_turn,
        })
        .collect())
}

impl Snapshot {
    /// The state this snapshot reads.
    pub fn mark(&self) -> Mark {
        Mark {
            items: self.items.len() as u64,
            lines: self.lines.len() as u64,
            sessions: self.tracks.len() as u64,
        }
    }

    /// How many items the main tree holds.
    pub fn len(&self) -> Result<u64> {
        Ok(jsonl::count(&self.items))
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.items.is_empty())
    }

    /// The item a new session's branch hangs on: the newest; `None` while there is none.
    pub fn graft(&self) -> Result<Option<u64>> {
        Ok(self.len()?.checked_sub(1))
    }

    /// The heads of the main tree's items, in order: position is sequence number.
    pub fn heads(&self) -> Result<Vec<Head>> {
        heads(&self.name, "item", &self.items)
    }

    /// The items of the main tree, in order: position is sequence number.
    pub fn items(&self) -> Result<Vec<Item>> {
        items(&self.name, "item", &self.items)
    }

    /// Whether each item of the main tree opens a turn, from its kind tag alone.
    fn opens(&self) -> Result<Vec<bool>> {
        jsonl::lines(&self.items)
            .enumerate()
            .map(|(i, l)| {
                let t: Tag = jsonl::decode(l)
                    .with_context(|| format!("Thread {}: item {i} is corrupt", self.name))?;
                Ok(t.opens())
            })
            .collect()
    }

    /// Item `seq` of the main tree.
    pub fn item(&self, seq: u64) -> Result<Item> {
        let row = |seq: u64| -> Result<Row> {
            let line = jsonl::lines(&self.items)
                .nth(seq as usize)
                .with_context(|| format!("No item {seq}."))?;
            let row: Row = jsonl::decode(line)
                .with_context(|| format!("Thread {}: item {seq} is corrupt", self.name))?;
            check_head(&self.name, "item", seq, &row.head)?;
            Ok(row)
        };
        let r = row(seq)?;
        let parent = r
            .head
            .parent
            .map(|p| row(p).map(|p| p.head.source()))
            .transpose()?;
        Ok(Item {
            id: r.head.source(),
            parent,
            ts: r.head.ts,
            kind: r.kind,
            ends_turn: r.ends_turn,
        })
    }

    /// The main tree: positions are sequence numbers.
    pub fn tree(&self) -> Result<Tree> {
        Ok(Tree::new(self.items()?))
    }

    /// The items of the main tree nothing follows, in order: the last is the newest item.
    pub fn tips(&self) -> Result<Vec<u64>> {
        Ok(tips(&self.heads()?))
    }

    /// The newest branch record of each session, by harness and session id; tips and grafts
    /// are checked against the items.
    fn tracks(&self) -> Result<BTreeMap<(String, String), Track>> {
        let n = self.len()?;
        let mut out = BTreeMap::new();
        for (i, l) in jsonl::lines(&self.tracks).enumerate() {
            let t: Track = jsonl::decode(l).with_context(|| {
                format!("Thread {}: session record {} is corrupt", self.name, i + 1)
            })?;
            for (what, seq) in [("tip", t.tip), ("graft", t.graft)] {
                if let Some(seq) = seq.filter(|&s| s >= n) {
                    bail!(
                        "Thread {}: session record {} is corrupt: {} {} has {what} {seq}, beyond its {n} items.",
                        self.name,
                        i + 1,
                        t.harness,
                        t.session
                    );
                }
            }
            out.insert((t.harness.clone(), t.session.clone()), t);
        }
        Ok(out)
    }

    /// Session `session`'s branch record in this thread, attached or not.
    pub(super) fn track(&self, harness: &str, session: &str) -> Result<Option<Track>> {
        Ok(self.tracks()?.remove(&(harness.into(), session.into())))
    }

    /// The changes of the attachments of sessions to this thread, in order.
    fn events(&self) -> Result<Vec<Event>> {
        events(&self.name, &self.events)
    }

    /// The binding of `t`, from its session's attachment changes `events`: when it was last
    /// attached here, and detached if a change followed, with its transcript's path. `None`
    /// until an attachment to this thread is recorded.
    fn bind(&self, t: Track, events: &[&Event]) -> Result<Option<Binding>> {
        let Some(i) = events.iter().rposition(|e| e.state == State::Attached) else {
            return Ok(None);
        };
        Ok(Some(Binding {
            attached: events[i].at,
            detached: events.get(i + 1).map(|e| e.at),
            harness: t.harness,
            session: t.session,
            transcript: t.transcript,
            config: t.config,
            graft: t.graft,
            tip: t.tip,
        }))
    }

    /// Every session bound to this thread, attached or not, by harness and session id.
    pub fn bindings(&self) -> Result<Vec<Binding>> {
        let events = self.events()?;
        let mut by: HashMap<(&str, &str), Vec<&Event>> = HashMap::new();
        for e in &events {
            by.entry((&e.harness, &e.session)).or_default().push(e);
        }
        let mut out = Vec::new();
        for t in self.tracks()?.into_values() {
            let Some(events) = by.get(&(t.harness.as_str(), t.session.as_str())).cloned() else {
                continue;
            };
            out.extend(self.bind(t, &events)?);
        }
        Ok(out)
    }

    pub fn binding(&self, harness: &str, session: &str) -> Result<Option<Binding>> {
        let Some(t) = self.track(harness, session)? else {
            return Ok(None);
        };
        let events = self.events()?;
        let mine: Vec<&Event> = events
            .iter()
            .filter(|e| e.harness == harness && e.session == session)
            .collect();
        self.bind(t, &mine)
    }

    /// The newest tip of a session: the item of its transcript's newest entry.
    pub fn tip(&self, harness: &str, session: &str) -> Result<Option<u64>> {
        Ok(self.binding(harness, session)?.and_then(|b| b.tip))
    }

    /// Every subagent, by harness and agent id, as its newest record holds it.
    pub fn agents(&self) -> Result<Vec<Agent>> {
        let n = self.len()?;
        let mut out: BTreeMap<(String, String), Agent> = BTreeMap::new();
        for (i, l) in jsonl::lines(&self.agents).enumerate() {
            let a: Agent = jsonl::decode(l).with_context(|| {
                format!("Thread {}: agent record {} is corrupt", self.name, i + 1)
            })?;
            check_id(&a.harness, "harness")?;
            check_id(&a.agent, "agent")?;
            if let Some(call) = a.call.filter(|&c| c >= n) {
                bail!(
                    "Thread {}: agent record {} is corrupt: agent {} has call {call}, beyond its {n} items.",
                    self.name,
                    i + 1,
                    a.agent
                );
            }
            out.insert((a.harness.clone(), a.agent.clone()), a);
        }
        Ok(out.into_values().collect())
    }

    pub fn agent(&self, harness: &str, agent: &str) -> Result<Option<Agent>> {
        Ok(self
            .agents()?
            .into_iter()
            .find(|a| a.harness == harness && a.agent == agent))
    }

    fn agent_path(&self, harness: &str, agent: &str) -> Result<PathBuf> {
        let (h, a) = (check_id(harness, "harness")?, check_id(agent, "agent")?);
        Ok(self.dir.join("agents").join(h).join(format!("{a}.jsonl")))
    }

    /// A subagent's items, in order; its roots follow its [`Agent::call`].
    pub fn agent_items(&self, harness: &str, agent: &str) -> Result<Vec<Item>> {
        let b = jsonl::read(&self.agent_path(harness, agent)?)?;
        items(&self.name, &format!("agent {agent} item"), &b)
    }

    pub fn line(&self, key: Key) -> Result<Option<Line>> {
        Ok(self.all_lines()?.remove(&key))
    }

    /// Every line, by key.
    pub fn all_lines(&self) -> Result<BTreeMap<Key, Line>> {
        let n = self.len()?;
        let mut out = BTreeMap::new();
        for (i, l) in jsonl::lines(&self.lines).enumerate() {
            let op: LineOp = jsonl::decode(l).with_context(|| {
                format!("Thread {}: line record {} is corrupt", self.name, i + 1)
            })?;
            match op {
                LineOp::Put { key, line } => {
                    if key.last >= n {
                        bail!(
                            "Thread {}: line record {} is corrupt: line {key} is beyond its {n} items.",
                            self.name,
                            i + 1
                        );
                    }
                    out.entry(key).or_insert(line);
                }
                LineOp::Drop { keys } => {
                    for k in keys {
                        out.remove(&k);
                    }
                }
                LineOp::Clear => out.clear(),
            }
        }
        Ok(out)
    }

    /// Every line of `level`, by the last item it covers.
    pub fn lines(&self, level: u32) -> Result<Vec<(u64, Line)>> {
        Ok(self
            .all_lines()?
            .into_iter()
            .filter(|(k, _)| k.level == level)
            .map(|(k, l)| (k.last, l))
            .collect())
    }

    /// Every version of the session files this thread read, in the order each began.
    pub fn versions(&self) -> Result<Vec<Version>> {
        source::decode(&self.name, &self.sources)
    }

    /// The records of `t`'s files this thread lacks, for session `b` on this machine: a file
    /// that grew is recorded at its new length, and one that no longer begins with what the
    /// thread read begins a new version. Then a warning for each file that no longer holds
    /// what the thread read of it: one that no longer begins with it, and one that is gone.
    fn point(&self, b: &Track, t: &Transcript) -> Result<(Vec<Pointer>, Vec<String>)> {
        const CHECK: &str = "`ai backup threads` tells whether the backup holds them";
        let host = backup::host()?;
        let at = Timestamp::now();
        let mine: Vec<Version> = self
            .versions()?
            .into_iter()
            .filter(|v| v.harness == b.harness && v.session == b.session && v.read.host == host)
            .collect();
        let live: BTreeMap<PathBuf, &[u8]> = t
            .files
            .iter()
            .map(|(rel, bytes)| (t.dir.join(rel), bytes.as_slice()))
            .collect();
        let mut out = Vec::new();
        let mut unheld = Vec::new();
        for (path, bytes) in &live {
            let newest = mine.iter().rfind(|v| v.read.path == *path);
            let (begins, sha256) = source::hashes(bytes, newest.map(|v| &v.read));
            let len = bytes.len() as u64;
            let from = match newest {
                Some(v) if begins && v.read.len == len => continue,
                Some(v) if begins => v.read.len,
                Some(v) => {
                    unheld.push(format!(
                        "Thread {}: {} no longer begins with the {} this thread read of it. \
                         {CHECK}.",
                        self.name,
                        pretty(path),
                        human(v.read.len)
                    ));
                    0
                }
                None => 0,
            };
            out.push(Pointer {
                harness: b.harness.clone(),
                session: b.session.clone(),
                host: host.clone(),
                path: path.clone(),
                from,
                len,
                sha256,
                at,
            });
        }
        let gone: BTreeMap<&Path, &Version> = mine
            .iter()
            .filter(|v| v.read.len > 0 && !live.contains_key(&v.read.path))
            .map(|v| (v.read.path.as_path(), v))
            .collect();
        for (path, v) in gone {
            unheld.push(format!(
                "Thread {}: {} is gone; this thread read {} of it. {CHECK}.",
                self.name,
                pretty(path),
                human(v.read.len)
            ));
        }
        Ok((out, unheld))
    }

    /// Adds the items of `t` this thread lacks, for session `b`, and records its files: a root
    /// hangs on `b.graft`, `b.tip` moves to its newest entry. Entries are known by harness and
    /// entry id. The caller holds `thread`'s writer lock and read this snapshot under it.
    pub(super) fn ingest(&self, thread: &Thread, b: &mut Track, t: &Transcript) -> Result<Added> {
        check_id(&b.harness, "harness")?;
        check_id(&b.session, "session")?;
        let (pointers, unheld) = self.point(b, t)?;
        thread.append(SOURCES, &pointers)?;
        let mut main = Append::new(self.heads()?);
        let first = main.len();
        for item in &t.items {
            if main.seq(&item.id).is_none() {
                let parent = item.parent.as_ref().and_then(|p| main.seq(p));
                main.push(item, parent.or(b.graft));
            }
        }
        if let Some(last) = t.items.last() {
            b.tip = main.seq(&last.id);
        }
        let mut agents = self.agents()?;
        let mut records = Vec::new();
        let mut agent_items = 0;
        for sub in &t.agents {
            let call = sub
                .call
                .as_deref()
                .and_then(|c| {
                    t.items
                        .iter()
                        .find(|i| matches!(&i.kind, Kind::ToolCall { id, .. } if id == c))
                })
                .and_then(|i| main.seq(&i.id));
            let path = self.agent_path(&b.harness, &sub.agent)?;
            let what = format!("agent {} item", sub.agent);
            let mut items = Append::new(heads(&self.name, &what, &jsonl::read(&path)?)?);
            for item in &sub.items {
                if items.seq(&item.id).is_none() {
                    let parent = item.parent.as_ref().and_then(|p| items.seq(p));
                    items.push(item, parent);
                }
            }
            agent_items += items.rows.len() as u64;
            if !items.rows.is_empty()
                && let Some(dir) = path.parent()
            {
                fs::create_dir_all(dir).at(dir)?;
            }
            jsonl::append(&path, &items.rows)?;
            let i = agents
                .iter()
                .position(|a| a.harness == b.harness && a.agent == sub.agent);
            let agent = Agent {
                harness: b.harness.clone(),
                agent: sub.agent.clone(),
                agent_type: sub.agent_type.clone(),
                description: sub.description.clone(),
                call: call.or_else(|| i.and_then(|i| agents[i].call)),
            };
            if i.map(|i| &agents[i]) != Some(&agent) {
                match i {
                    Some(i) => agents[i] = agent.clone(),
                    None => agents.push(agent.clone()),
                }
                records.push(agent);
            }
        }
        let added = first..main.len();
        thread.append(ITEMS, &main.rows)?;
        thread.append(AGENTS, &records)?;
        Ok(Added {
            items: added,
            agent_items,
            unheld,
        })
    }
}

/// New rows of one item sequence (the main tree's, or a subagent's).
struct Append {
    held: u64,
    keys: HashMap<(String, String), u64>,
    rows: Vec<Row>,
}

impl Append {
    fn new(heads: Vec<Head>) -> Append {
        Append {
            held: heads.len() as u64,
            keys: heads
                .into_iter()
                .map(|h| ((h.harness, h.entry), h.seq))
                .collect(),
            rows: Vec::new(),
        }
    }

    fn len(&self) -> u64 {
        self.held + self.rows.len() as u64
    }

    /// The number of the item from `id`'s entry, if the sequence holds it.
    fn seq(&self, id: &SourceId) -> Option<u64> {
        self.keys.get(&key(id)).copied()
    }

    fn push(&mut self, item: &Item, parent: Option<u64>) {
        let seq = self.len();
        self.rows.push(Row {
            head: Head {
                seq,
                parent,
                harness: item.id.harness.clone(),
                session: item.id.session.clone(),
                entry: item.id.entry.clone(),
                ts: item.ts,
            },
            kind: item.kind.clone(),
            ends_turn: item.ends_turn,
        });
        self.keys.insert(key(&item.id), seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    use crate::agent::model::turns;

    #[test]
    fn turn_counts_match_the_tree() {
        // (parent, opens): a root that does not open, two branches, a second root that opens.
        let spec = [
            (None, false),
            (Some(0), true),
            (Some(1), false),
            (Some(2), true),
            (Some(0), true),
            (Some(4), false),
            (None, true),
        ];
        let heads: Vec<Head> = (0..)
            .zip(spec)
            .map(|(seq, (parent, _))| Head {
                seq,
                parent,
                harness: "test".into(),
                session: "s".into(),
                entry: seq.to_string(),
                ts: Timestamp::UNIX_EPOCH,
            })
            .collect();
        let opens: Vec<bool> = spec.iter().map(|s| s.1).collect();
        let tree = Tree::new(
            heads
                .iter()
                .zip(&opens)
                .map(|(h, &o)| Item {
                    id: h.source(),
                    parent: h.parent.map(|p| heads[p as usize].source()),
                    ts: h.ts,
                    kind: if o {
                        Kind::User(String::new())
                    } else {
                        Kind::Text(String::new())
                    },
                    ends_turn: false,
                })
                .collect(),
        );
        for tip in tips(&heads) {
            assert_eq!(
                count_turns(&heads, &opens, tip),
                turns(&tree.path(tip as usize)).len(),
                "tip {tip}"
            );
        }
    }

    /// The agents of a project at `dir`, with `agents`, its homes in `dir`.
    fn store(dir: &Path, agents: &[&str]) -> Store {
        let homes = Homes::at(&dir.join("home"));
        Project::init_in(dir, &homes).unwrap();
        let s = Store::find_in(&homes, dir).unwrap().unwrap();
        for a in agents {
            s.create(a).unwrap();
        }
        s
    }

    /// Attaches session `session` to `name` with the plain fixture as its transcript.
    fn attach(s: &Store, dir: &Path, name: &str, session: &str) -> PathBuf {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/plain.jsonl");
        let p = dir.join(format!("{session}.jsonl"));
        fs::copy(fixture, &p).unwrap();
        s.attach(name, "claude", session, Some(&p)).unwrap();
        p
    }

    fn names(s: &Store) -> Vec<String> {
        s.threads()
            .unwrap()
            .iter()
            .map(|t| t.name.clone())
            .collect()
    }

    fn err<T>(r: Result<T>) -> String {
        r.err().expect("an error").to_string()
    }

    fn quiet(t: &str) {
        panic!("waited for {t}'s compactor");
    }

    /// An event attaching `session` to `t`, written as attaching does, under its lock.
    fn attach_event(t: &Thread, session: &str) {
        let e = Event {
            harness: "claude".into(),
            session: session.into(),
            at: Timestamp::now(),
            state: State::Attached,
        };
        t.append(ATTACHMENTS, &[e]).unwrap();
    }

    #[test]
    fn rename_moves_the_thread_with_its_sessions_and_log() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev", "other"]);
        attach(&s, dir.path(), "dev", "s-1");
        let dev = s.thread("dev").unwrap();
        fs::write(dev.log_path(), "dev's log\n").unwrap();
        let items = dev.snapshot().unwrap().items().unwrap();

        let t = s.rename("dev", "reviewer", quiet).unwrap();
        assert_eq!(t.name(), "reviewer");
        assert_eq!(names(&s), ["other", "reviewer"]);
        assert!(!dev.exists().unwrap());
        assert_eq!(t.snapshot().unwrap().items().unwrap(), items);
        let (owner, b) = s.attached("claude", "s-1").unwrap().unwrap();
        assert_eq!(owner.name(), "reviewer");
        assert!(b.is_attached());
        assert_eq!(fs::read_to_string(t.log_path()).unwrap(), "dev's log\n");
        assert!(!dev.log_path().exists());
    }

    #[test]
    fn rename_refuses_a_missing_agent_a_taken_name_and_a_bad_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev", "two"]);
        assert_eq!(
            err(s.rename("nope", "x", quiet)),
            "No agent nope in this project. Create it: ai agent new nope"
        );
        assert_eq!(
            err(s.rename("dev", "dev", quiet)),
            "Agent dev is already called dev."
        );
        assert_eq!(
            err(s.rename("dev", "two", quiet)),
            "Agent two already exists in this project."
        );
        assert_eq!(
            err(s.rename("dev", "a b", quiet)),
            "'a b' is not a thread name: letters, digits, '-' and '_'."
        );
        assert_eq!(names(&s), ["dev", "two"]);
    }

    #[test]
    fn rename_never_replaces_an_agent_made_after_its_check() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        let w = s.thread("dev").unwrap().writer().unwrap();
        let path = dir.path().to_path_buf();
        let renaming = thread::spawn(move || {
            let s = Store::find_in(&Homes::at(&path.join("home")), &path)
                .unwrap()
                .unwrap();
            s.rename("dev", "late", quiet).map(|t| t.name)
        });
        thread::sleep(Duration::from_millis(200));
        s.create("late").unwrap();
        drop(w);
        assert_eq!(
            err(renaming.join().unwrap()),
            "Agent late already exists in this project."
        );
        assert_eq!(names(&s), ["dev", "late"]);
    }

    #[test]
    fn a_thread_found_before_its_rename_is_never_written_or_remade() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        attach(&s, dir.path(), "dev", "s-1");
        let stale = s.thread("dev").unwrap();
        s.rename("dev", "reviewer", quiet).unwrap();

        let gone = "Agent dev was renamed or removed while this ran.";
        let line = stale.put_line(
            Key { level: 1, last: 0 },
            LineKind::Turn,
            &Who::default(),
            "x",
        );
        assert_eq!(err(line), gone);
        assert_eq!(err(stale.drop_lines()), gone);
        assert_eq!(err(stale.try_lock()), gone);
        assert_eq!(err(stale.lock()), gone);
        assert_eq!(err(stale.lock_before(Instant::now())), gone);
        assert_eq!(err(stale.record("claude", "s-2", State::Attached)), gone);
        assert!(stale.snapshot().unwrap().is_empty().unwrap());
        assert!(!stale.exists().unwrap());
        assert_eq!(names(&s), ["reviewer"]);
    }

    #[test]
    fn rename_waits_for_the_compactor_and_the_writer() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        let dev = s.thread("dev").unwrap();
        let compacting = dev.lock().unwrap();
        let writing = dev.writer().unwrap();
        let (heard, waited) = mpsc::channel();
        let path = dir.path().to_path_buf();
        let renaming = thread::spawn(move || {
            let s = Store::find_in(&Homes::at(&path.join("home")), &path)
                .unwrap()
                .unwrap();
            s.rename("dev", "reviewer", |t| heard.send(t.to_string()).unwrap())
                .map(|t| t.name)
        });
        assert_eq!(waited.recv_timeout(Duration::from_secs(5)).unwrap(), "dev");
        drop(compacting);
        thread::sleep(Duration::from_millis(200));
        assert!(dev.exists().unwrap(), "it moved under the writer's lock");
        drop(writing);
        assert_eq!(renaming.join().unwrap().unwrap(), "reviewer");
        assert_eq!(names(&s), ["reviewer"]);
    }

    #[test]
    fn rename_waits_for_a_session_attached_while_it_waited() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        let dev = s.thread("dev").unwrap();
        let writing = dev.writer().unwrap();
        let path = dir.path().to_path_buf();
        let renaming = thread::spawn(move || {
            let s = Store::find_in(&Homes::at(&path.join("home")), &path)
                .unwrap()
                .unwrap();
            s.rename("dev", "reviewer", quiet).map(|t| t.name)
        });
        thread::sleep(Duration::from_millis(200));
        let session = s.session_lock("claude", "s-2").unwrap();
        attach_event(&dev, "s-2");
        drop(writing);
        thread::sleep(Duration::from_millis(200));
        assert!(dev.exists().unwrap(), "it moved under the session's lock");
        drop(session);
        assert_eq!(renaming.join().unwrap().unwrap(), "reviewer");
        assert_eq!(
            s.owner("claude", "s-2").unwrap().as_deref(),
            Some("reviewer")
        );
    }

    fn no(_: &[(String, String)]) -> Result<()> {
        Ok(())
    }

    /// The files under `path` in commit `commit` of the repository at `dir`, with their bytes.
    fn committed(dir: &Path, commit: &str, path: &Path) -> BTreeMap<String, Vec<u8>> {
        let repo = gix::open(dir).unwrap();
        let id = gix::ObjectId::from_hex(commit.as_bytes()).unwrap();
        let tree = repo.find_commit(id).unwrap().tree().unwrap();
        let mut out = BTreeMap::new();
        for e in tree.traverse().breadthfirst.files().unwrap() {
            let p = e.filepath.to_string();
            if e.mode.is_blob() && p.starts_with(path.to_str().unwrap()) {
                out.insert(p, repo.find_blob(e.oid).unwrap().take_data());
            }
        }
        out
    }

    /// The files under `dir`, by their path relative to `repo`, with their bytes.
    fn on_disk(repo: &Path, dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        for e in ignore::WalkBuilder::new(dir)
            .standard_filters(false)
            .build()
        {
            let e = e.unwrap();
            if e.file_type().unwrap().is_file() {
                let rel = e.path().strip_prefix(repo).unwrap();
                out.insert(rel.to_str().unwrap().into(), fs::read(e.path()).unwrap());
            }
        }
        out
    }

    /// The removed logs of `t` on this machine, by their contents.
    fn retired(t: &Thread) -> Vec<String> {
        let mut logs: Vec<String> = fs::read_dir(&t.local)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("compact.removed-")
            })
            .map(|p| fs::read_to_string(p).unwrap())
            .collect();
        logs.sort();
        logs
    }

    #[test]
    fn remove_takes_the_thread_out_and_a_commit_keeps_its_last_state() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev", "other"]);
        attach(&s, dir.path(), "dev", "s-1");
        let dev = s.thread("dev").unwrap();
        fs::write(dev.log_path(), "dev's log\n").unwrap();
        let repo = s.homes().repo.clone();
        let before = on_disk(&repo, &dev.dir);
        assert!(!before.is_empty());
        let seen = std::cell::RefCell::new(Vec::new());

        let removed = s
            .remove("dev", quiet, |sessions| {
                seen.borrow_mut().extend_from_slice(sessions);
                Ok(())
            })
            .unwrap();
        assert_eq!(*seen.borrow(), [("claude".to_string(), "s-1".to_string())]);
        assert_eq!(names(&s), ["other"]);
        assert!(!dev.exists().unwrap());
        assert_eq!(s.owner("claude", "s-1").unwrap(), None);
        assert_eq!(
            repo.join(&removed.path),
            Store::root(s.homes(), s.project())
                .unwrap()
                .join("threads/dev")
        );
        let commit = removed.commit.clone().unwrap();
        assert_eq!(committed(&repo, &commit, &removed.path), before);
        assert!(!dev.log_path().exists());
        assert_eq!(retired(&dev), ["dev's log\n"]);

        let copies = || {
            fs::read_dir(repo::git_dir(&repo))
                .unwrap()
                .filter(|e| {
                    let e = e.as_ref().unwrap();
                    e.file_name().to_str().unwrap().starts_with("ai-remove-")
                })
                .count()
        };
        assert_eq!(copies(), 1);
        removed.delete().unwrap();
        assert_eq!(copies(), 0);
        s.create("dev").unwrap();
        assert!(dev.snapshot().unwrap().is_empty().unwrap());
    }

    #[test]
    fn remove_stops_at_a_refusal_and_changes_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        attach(&s, dir.path(), "dev", "s-1");
        let dev = s.thread("dev").unwrap();
        fs::write(dev.log_path(), "dev's log\n").unwrap();
        let repo = s.homes().repo.clone();
        let head = || gix::open(&repo).unwrap().head_id().ok().map(|i| i.detach());
        let was = head();

        let r = s.remove("dev", quiet, |_| bail!("It runs."));
        assert_eq!(err(r), "It runs.");
        assert_eq!(names(&s), ["dev"]);
        assert_eq!(head(), was);
        assert_eq!(fs::read_to_string(dev.log_path()).unwrap(), "dev's log\n");
        assert_eq!(s.owner("claude", "s-1").unwrap().as_deref(), Some("dev"));
        assert_eq!(
            err(s.remove("nope", quiet, no)),
            "No agent nope in this project. Create it: ai agent new nope"
        );
    }

    #[test]
    fn remove_of_a_thread_that_holds_no_file_names_no_commit() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        let removed = s.remove("dev", quiet, no).unwrap();
        assert_eq!(removed.commit, None);
        assert!(names(&s).is_empty());
    }

    #[test]
    fn remove_refuses_a_thread_holding_a_file_git_leaves_out() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        attach(&s, dir.path(), "dev", "s-1");
        let dev = s.thread("dev").unwrap();
        fs::write(s.homes().repo.join(".gitignore"), "*.tmp\n").unwrap();
        let kept = dev.dir.join("notes.tmp");
        fs::write(&kept, "mine\n").unwrap();
        let e = err(s.remove("dev", quiet, no));
        assert!(
            e.starts_with("Cannot remove agent dev: git leaves out ")
                && e.contains("notes.tmp")
                && e.ends_with("Move it out of the thread, then try again."),
            "{e}"
        );
        assert_eq!(fs::read_to_string(kept).unwrap(), "mine\n");
        assert_eq!(names(&s), ["dev"]);
    }

    #[test]
    fn a_thread_found_before_its_removal_is_never_written_or_remade() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        attach(&s, dir.path(), "dev", "s-1");
        let stale = s.thread("dev").unwrap();
        s.remove("dev", quiet, no).unwrap().delete().unwrap();

        let gone = "Agent dev was renamed or removed while this ran.";
        let line = stale.put_line(
            Key { level: 1, last: 0 },
            LineKind::Turn,
            &Who::default(),
            "x",
        );
        assert_eq!(err(line), gone);
        assert_eq!(err(stale.lock()), gone);
        assert_eq!(err(stale.record("claude", "s-2", State::Attached)), gone);
        assert!(!stale.exists().unwrap());
        assert!(names(&s).is_empty());
    }

    #[test]
    fn remove_waits_for_the_compactor_and_hears_a_session_attached_meanwhile() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        let dev = s.thread("dev").unwrap();
        let compacting = dev.lock().unwrap();
        let writing = dev.writer().unwrap();
        let (heard, waited) = mpsc::channel();
        let (told, sessions) = mpsc::channel();
        let path = dir.path().to_path_buf();
        let removing = thread::spawn(move || {
            let s = Store::find_in(&Homes::at(&path.join("home")), &path)
                .unwrap()
                .unwrap();
            s.remove(
                "dev",
                |t| heard.send(t.to_string()).unwrap(),
                |a| {
                    told.send(a.to_vec()).unwrap();
                    Ok(())
                },
            )
            .map(|r| r.commit)
        });
        assert_eq!(waited.recv_timeout(Duration::from_secs(5)).unwrap(), "dev");
        drop(compacting);
        thread::sleep(Duration::from_millis(200));
        let session = s.session_lock("claude", "s-2").unwrap();
        attach_event(&dev, "s-2");
        drop(writing);
        thread::sleep(Duration::from_millis(200));
        assert!(dev.exists().unwrap(), "it went under the session's lock");
        drop(session);
        assert!(removing.join().unwrap().unwrap().is_some());
        assert_eq!(
            sessions.recv().unwrap(),
            [("claude".to_string(), "s-2".to_string())]
        );
        assert!(names(&s).is_empty());
    }

    #[test]
    fn each_removal_keeps_its_own_log() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        let dev = s.thread("dev").unwrap();
        fs::write(dev.local_file(COMPACT_LOG).unwrap(), "first\n").unwrap();
        s.remove("dev", quiet, no).unwrap().delete().unwrap();
        s.create("dev").unwrap();
        fs::write(dev.local_file(COMPACT_LOG).unwrap(), "second\n").unwrap();
        s.remove("dev", quiet, no).unwrap().delete().unwrap();
        assert_eq!(retired(&dev), ["first\n", "second\n"]);
    }

    #[test]
    fn rename_moves_aside_a_log_left_under_the_new_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        let (dev, reviewer) = (s.thread("dev").unwrap(), s.thread("reviewer").unwrap());
        fs::write(dev.local_file(COMPACT_LOG).unwrap(), "dev\n").unwrap();
        fs::write(
            reviewer.local_file(COMPACT_LOG).unwrap(),
            "an earlier reviewer\n",
        )
        .unwrap();
        s.rename("dev", "reviewer", quiet).unwrap();
        assert_eq!(fs::read_to_string(reviewer.log_path()).unwrap(), "dev\n");
        assert_eq!(retired(&reviewer), ["an earlier reviewer\n"]);
        assert!(!dev.log_path().exists());
    }

    #[test]
    fn a_new_agent_starts_its_own_log_whatever_its_name_left_behind() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        s.rename("dev", "reviewer", quiet).unwrap();
        let dev = s.thread("dev").unwrap();
        fs::write(
            dev.local_file(COMPACT_LOG).unwrap(),
            "a summariser of the renamed dev\n",
        )
        .unwrap();
        s.create("dev").unwrap();
        assert!(!dev.log_path().exists());
        assert_eq!(retired(&dev), ["a summariser of the renamed dev\n"]);

        fs::write(dev.log_path(), "dev's own\n").unwrap();
        assert!(err(s.create("dev")).contains("already exists"));
        assert_eq!(fs::read_to_string(dev.log_path()).unwrap(), "dev's own\n");
    }

    /// Runs `op` while `t`'s files on this machine can't be moved or made: its directory there
    /// allows no change, its lock files made first.
    fn with_fixed_local<T>(t: &Thread, op: impl FnOnce() -> T) -> T {
        use std::os::unix::fs::PermissionsExt;
        for lock in [COMPACT_LOCK, WRITE_LOCK] {
            fs::write(t.local_file(lock).unwrap(), "").unwrap();
        }
        fs::set_permissions(&t.local, fs::Permissions::from_mode(0o555)).unwrap();
        let out = op();
        fs::set_permissions(&t.local, fs::Permissions::from_mode(0o755)).unwrap();
        out
    }

    #[test]
    fn a_rename_stands_when_its_log_cannot_move() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        let dev = s.thread("dev").unwrap();
        fs::write(dev.local_file(COMPACT_LOG).unwrap(), "dev\n").unwrap();
        let renamed = with_fixed_local(&dev, || s.rename("dev", "reviewer", quiet));
        assert_eq!(renamed.unwrap().name(), "reviewer");
        assert_eq!(names(&s), ["reviewer"]);
        assert_eq!(fs::read_to_string(dev.log_path()).unwrap(), "dev\n");
    }

    #[test]
    fn a_removal_stands_when_its_log_cannot_move() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        attach(&s, dir.path(), "dev", "s-1");
        let dev = s.thread("dev").unwrap();
        fs::write(dev.local_file(COMPACT_LOG).unwrap(), "dev\n").unwrap();
        let removed = with_fixed_local(&dev, || s.remove("dev", quiet, no));
        assert!(removed.unwrap().commit.is_some());
        assert!(names(&s).is_empty());
        assert_eq!(fs::read_to_string(dev.log_path()).unwrap(), "dev\n");
    }

    #[test]
    fn a_removal_that_fails_keeps_the_log_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        attach(&s, dir.path(), "dev", "s-1");
        let dev = s.thread("dev").unwrap();
        fs::write(dev.local_file(COMPACT_LOG).unwrap(), "dev\n").unwrap();
        s.commit("everything so far").unwrap();
        let git = repo::git_dir(&s.homes().repo);
        fs::set_permissions(&git, fs::Permissions::from_mode(0o555)).unwrap();
        let e = s.remove("dev", quiet, no).map(drop);
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err(e).contains("Cannot make a directory to remove the thread in"));
        assert_eq!(names(&s), ["dev"]);
        assert_eq!(fs::read_to_string(dev.log_path()).unwrap(), "dev\n");
        assert!(retired(&dev).is_empty());
    }

    #[test]
    fn restore_brings_a_removed_thread_back_as_its_commit_holds_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev", "other"]);
        attach(&s, dir.path(), "dev", "s-1");
        let dev = s.thread("dev").unwrap();
        let repo = s.homes().repo.clone();
        let before = on_disk(&repo, &dev.dir);
        let removed = s.remove("dev", quiet, no).unwrap();
        let commit = removed.commit.clone().unwrap();
        removed.delete().unwrap();
        assert_eq!(names(&s), ["other"]);

        let restored = s.restore("dev", &commit).unwrap();
        assert_eq!(restored.detached, []);
        assert_eq!(on_disk(&repo, &dev.dir), before);
        let (owner, b) = s.attached("claude", "s-1").unwrap().unwrap();
        assert_eq!(owner.name(), "dev");
        assert!(b.is_attached());
        let left: Vec<_> = fs::read_dir(repo::git_dir(&repo))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("ai-restore-"))
            .collect();
        assert_eq!(left, [""; 0]);
    }

    #[test]
    fn restore_never_replaces_an_agent_made_under_the_name_since() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        attach(&s, dir.path(), "dev", "s-1");
        let removed = s.remove("dev", quiet, no).unwrap();
        let commit = removed.commit.clone().unwrap();
        removed.delete().unwrap();
        s.create("dev").unwrap();
        attach(&s, dir.path(), "dev", "s-2");
        let dev = s.thread("dev").unwrap();
        let repo = s.homes().repo.clone();
        let made = on_disk(&repo, &dev.dir);

        let e = err(s.restore("dev", &commit));
        assert_eq!(
            e,
            "Agent dev exists in this project, and restoring would replace it. Rename it first: \
             ai agent rename dev <new name>"
        );
        assert_eq!(on_disk(&repo, &dev.dir), made);

        s.rename("dev", "newer", quiet).unwrap();
        s.restore("dev", &commit).unwrap();
        assert_eq!(names(&s), ["dev", "newer"]);
        assert_eq!(s.owner("claude", "s-1").unwrap().as_deref(), Some("dev"));
        assert_eq!(s.owner("claude", "s-2").unwrap().as_deref(), Some("newer"));
    }

    #[test]
    fn restore_leaves_a_session_with_the_thread_that_holds_it_now() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev", "other"]);
        let transcript = attach(&s, dir.path(), "dev", "s-1");
        let removed = s.remove("dev", quiet, no).unwrap();
        let commit = removed.commit.clone().unwrap();
        removed.delete().unwrap();
        s.attach("other", "claude", "s-1", Some(&transcript))
            .unwrap();

        let restored = s.restore("dev", &commit).unwrap();
        assert_eq!(
            restored.detached,
            [("claude".into(), "s-1".into(), "other".into())]
        );
        assert_eq!(s.owner("claude", "s-1").unwrap().as_deref(), Some("other"));
        let b = s.thread("dev").unwrap().snapshot().unwrap();
        let b = b.binding("claude", "s-1").unwrap().unwrap();
        assert!(!b.is_attached());
    }

    #[test]
    fn restore_refuses_a_commit_that_holds_no_such_agent() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = store(dir.path(), &["dev"]);
        attach(&s, dir.path(), "dev", "s-1");
        let removed = s.remove("dev", quiet, no).unwrap();
        let commit = removed.commit.clone().unwrap();
        removed.delete().unwrap();
        s.commit("the removal").unwrap();
        let head = gix::open(&s.homes().repo)
            .unwrap()
            .head_id()
            .unwrap()
            .to_string();
        assert_ne!(head, commit);

        assert!(err(s.restore("dev", &head)).ends_with("holds no agent dev of this project."));
        assert!(err(s.restore("dev", "abc")).contains("is not a commit's id"));
        let absent = "0".repeat(40);
        assert!(err(s.restore("dev", &absent)).contains("holds no commit"));
        assert!(names(&s).is_empty());
        s.restore("dev", &commit).unwrap();
        assert_eq!(names(&s), ["dev"]);
    }
}
