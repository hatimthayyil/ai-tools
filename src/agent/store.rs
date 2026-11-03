use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    ops::Range,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use crate::{
    agent::{
        claude::Subagent,
        convert,
        model::{Item, Kind, SourceId, Tree},
        record::{
            Agent, Binding, Event, Head, Key, Line, LineKind, LineOp, Pointer, Row, State, Tag,
            Track, check_id,
        },
        source,
    },
    config::Homes,
    file::{self, AtPath, pretty},
    jsonl,
    memory::{CALLS, Who},
    project::Project,
    repo, segments,
};

/// The agent store format this ai reads and writes.
pub const FORMAT: u32 = 6;
/// The agent store's directory in a project's folder of `~/ai`, and of this machine's files for
/// the project in `~/.ai`.
pub const DIR: &str = "agents";
pub(super) const THREADS: &str = "threads";
pub(super) const ITEMS: &str = "items.jsonl";
pub(super) const LINES: &str = "lines.jsonl";
pub(super) const SESSIONS: &str = "sessions.jsonl";
pub(super) const ATTACHMENTS: &str = "attachments.jsonl";
pub(super) const AGENTS: &str = "agents.jsonl";
pub(super) const SOURCES: &str = "sources.jsonl";
/// The lock every writer of a thread's files takes.
const WRITE_LOCK: &str = "write.lock";
/// The compactor's lock on a thread, and its log.
const COMPACT_LOCK: &str = "compact.lock";
const COMPACT_LOG: &str = "compact.log";

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
    /// project. A store of another format, or agents an older ai kept, are refused.
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
        convert::refuse_pending(&project, homes)?;
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
    /// attachment attached it. An error if several did, which no command of ai records.
    pub(super) fn owner(&self, harness: &str, session: &str) -> Result<Option<String>> {
        let mut owners = Vec::new();
        for t in self.threads()? {
            let last = t
                .snapshot()?
                .events()?
                .into_iter()
                .rfind(|e| e.harness == harness && e.session == session);
            if last.is_some_and(|e| e.state == State::Attached) {
                owners.push(t.name);
            }
        }
        match owners.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one.clone())),
            many => bail!(
                "Session {harness} {session} is attached to {}. Detach it from all but one.",
                many.join(" and ")
            ),
        }
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

    /// Creates agent `name` with an empty thread; an error if it exists.
    pub fn create(&self, name: &str) -> Result<Thread> {
        let t = self.thread(name)?;
        self.ensure()?;
        let threads = self.root.join(THREADS);
        fs::create_dir_all(&threads).at(&threads)?;
        match fs::create_dir(&t.dir) {
            Ok(()) => Ok(t),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                bail!("Agent {name} already exists in this project.")
            }
            Err(e) => Err(e).at(&t.dir),
        }
    }

    /// Commits the project's folder of `~/ai` as one commit `msg`, leaving the rest of the
    /// repository as it is; nothing if the folder did not change or does not exist.
    pub fn commit(&self, msg: &str) -> Result<()> {
        let folder = self.project.folder(&self.homes)?;
        if !folder.exists() {
            return Ok(());
        }
        let rel = folder
            .strip_prefix(&self.homes.repo)
            .expect("a project's folder is in the user's repository");
        repo::commit_paths(&self.homes.repo, &[rel], "ai", msg)
            .with_context(|| format!("Cannot commit {}", pretty(&folder)))
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
    /// the writer's lock.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let events = segments::read(&self.dir.join(ATTACHMENTS))?;
        let tracks = segments::read(&self.dir.join(SESSIONS))?;
        let sources = segments::read(&self.dir.join(SOURCES))?;
        let lines = segments::read(&self.dir.join(LINES))?;
        let agents = segments::read(&self.dir.join(AGENTS))?;
        Ok(Snapshot {
            name: self.name.clone(),
            dir: self.dir.clone(),
            items: segments::read(&self.dir.join(ITEMS))?,
            lines,
            tracks,
            sources,
            agents,
            events,
        })
    }

    /// The writers' lock on this thread's files.
    pub(super) fn writer(&self) -> Result<file::Lock> {
        file::lock(&self.local_file(WRITE_LOCK)?)
    }

    /// The file `name` among this machine's files for the thread, their directory made.
    fn local_file(&self, name: &str) -> Result<PathBuf> {
        fs::create_dir_all(&self.local).at(&self.local)?;
        Ok(self.local.join(name))
    }

    fn append<T: Serialize>(&self, file: &str, rows: &[T]) -> Result<()> {
        segments::append(&self.dir.join(file), rows)
    }

    /// Records a session's branch; the caller holds the writer's lock.
    pub(super) fn put_track(&self, t: &Track) -> Result<()> {
        self.append(SESSIONS, std::slice::from_ref(t))
    }

    /// Records a change of a session's attachment to this thread; the caller holds the
    /// session's lock.
    pub(super) fn record(&self, harness: &str, session: &str, state: State) -> Result<()> {
        let _w = self.writer()?;
        let event = Event {
            harness: harness.into(),
            session: session.into(),
            at: Timestamp::now(),
            state,
        };
        self.append(ATTACHMENTS, &[event])
    }

    /// The compactor's lock on this thread; `None` while another process holds it.
    pub fn try_lock(&self) -> Result<Option<file::Lock>> {
        file::try_lock(&self.local_file(COMPACT_LOCK)?)
    }

    /// The compactor's lock on this thread, waiting for it however long another process holds
    /// it.
    pub fn lock(&self) -> Result<file::Lock> {
        file::lock(&self.local_file(COMPACT_LOCK)?)
    }

    /// The compactor's lock on this thread, waiting for it until `deadline`; `None` if another
    /// process still holds it then.
    pub fn lock_before(&self, deadline: Instant) -> Result<Option<file::Lock>> {
        file::lock_before(&self.local_file(COMPACT_LOCK)?, deadline)
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
        jsonl::decode_all(&self.events)
            .with_context(|| format!("Thread {}: the attachments file is corrupt", self.name))
    }

    /// The binding of `t`, from its session's attachment changes `events`: when it was last
    /// attached here, and detached if a change followed, with its transcript's path. `None`
    /// until an attachment to this thread is recorded.
    fn bind(&self, t: Track, events: &[&Event]) -> Result<Option<Binding>> {
        let Some(i) = events.iter().rposition(|e| e.state == State::Attached) else {
            return Ok(None);
        };
        let home = source::home_here(&t.harness);
        let transcript = t
            .transcript
            .map(|p| source::locate(&t.harness, home.as_deref(), &p))
            .transpose()?;
        Ok(Some(Binding {
            attached: events[i].at,
            detached: events.get(i + 1).map(|e| e.at),
            harness: t.harness,
            session: t.session,
            transcript,
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
        let b = segments::read(&self.agent_path(harness, agent)?)?;
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

    /// The newest record of each file of a session this thread read, by harness and path.
    pub(super) fn pointers(&self) -> Result<BTreeMap<(String, PathBuf), Pointer>> {
        let mut out = BTreeMap::new();
        for (i, l) in jsonl::lines(&self.sources).enumerate() {
            let p: Pointer = jsonl::decode(l).with_context(|| {
                format!("Thread {}: source record {} is corrupt", self.name, i + 1)
            })?;
            out.insert((p.harness.clone(), p.path.clone()), p);
        }
        Ok(out)
    }

    /// The records of `t`'s files this thread lacks, for session `b`: a file is recorded again
    /// when it grew or changed. An error if a file it recorded is gone, or if the transcript no
    /// longer begins with the bytes it recorded: the harness's files are the only copy, so the
    /// error says to restore them.
    fn point(&self, b: &Track, t: &Transcript) -> Result<Vec<Pointer>> {
        let home = source::home_here(&b.harness);
        let mut known: BTreeMap<PathBuf, Pointer> = self
            .pointers()?
            .into_values()
            .filter(|p| p.harness == b.harness && p.session == b.session)
            .map(|p| (p.path.clone(), p))
            .collect();
        let mut out = Vec::new();
        for (relative, bytes) in &t.files {
            let abs = t.dir.join(relative);
            let path = source::point(home.as_deref(), &abs);
            let hash = source::hash(bytes)?;
            if let Some(old) = known.remove(&path) {
                if old.len == bytes.len() as u64 && old.hash == hash {
                    continue;
                }
                let kept = usize::try_from(old.len).ok().and_then(|n| bytes.get(..n));
                let prefix = kept.map(source::hash).transpose()?;
                if *relative == t.main && prefix.as_deref() != Some(old.hash.as_str()) {
                    bail!(
                        "Thread {}: {} no longer begins with the {} bytes this thread read from \
                         it. Restore it from your backup of ~/.ai/harness.",
                        self.name,
                        pretty(&abs),
                        old.len
                    );
                }
            }
            out.push(Pointer {
                harness: b.harness.clone(),
                session: b.session.clone(),
                path,
                len: bytes.len() as u64,
                hash,
            });
        }
        if let Some(gone) = known.into_values().next() {
            let abs = source::locate(&b.harness, home.as_deref(), &gone.path)?;
            bail!(
                "Thread {}: {} is gone; this thread read {} bytes of it. Restore it from your \
                 backup of ~/.ai/harness.",
                self.name,
                pretty(&abs),
                gone.len
            );
        }
        Ok(out)
    }

    /// Adds the items of `t` this thread lacks, for session `b`, and records its files: a root
    /// hangs on `b.graft`, `b.tip` moves to its newest entry. Entries are known by harness and
    /// entry id. The caller holds `thread`'s writer lock and read this snapshot under it.
    pub(super) fn ingest(&self, thread: &Thread, b: &mut Track, t: &Transcript) -> Result<Added> {
        check_id(&b.harness, "harness")?;
        check_id(&b.session, "session")?;
        thread.append(SOURCES, &self.point(b, t)?)?;
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
            let mut items = Append::new(heads(&self.name, &what, &segments::read(&path)?)?);
            for item in &sub.items {
                if items.seq(&item.id).is_none() {
                    let parent = item.parent.as_ref().and_then(|p| items.seq(p));
                    items.push(item, parent);
                }
            }
            agent_items += items.rows.len() as u64;
            segments::append(&path, &items.rows)?;
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
}
