use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    ops::Range,
    path::{Path, PathBuf},
    str::FromStr,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use crate::{
    chat::{
        claude::Subagent,
        model::{Item, Kind, SourceId, Tree},
        record::{
            Agent, Binding, Event, Head, Key, Line, LineKind, LineOp, Row, State, Tag, Track,
            check_id,
        },
    },
    file::{self, AtPath},
    jsonl,
    memory::{CALLS, Who, store_dir},
    project::Project,
};

/// The chat store format this ai reads and writes.
pub const FORMAT: u32 = 3;
const THREADS: &str = "threads";
const ITEMS: &str = "items.jsonl";
const LINES: &str = "lines.jsonl";
const SESSIONS: &str = "sessions.jsonl";
const AGENTS: &str = "agents.jsonl";
const SOURCES: &str = "sources";

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

/// A session as its harness's adapter reads it.
#[derive(Debug, Default)]
pub struct Transcript {
    pub items: Vec<Item>,
    pub agents: Vec<Subagent>,
    pub files: BTreeMap<PathBuf, Vec<u8>>,
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

impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", self.items, self.lines, self.sessions)
    }
}

impl FromStr for Mark {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Mark> {
        let n: Vec<u64> = s
            .split('.')
            .map(str::parse)
            .collect::<Result<Vec<u64>, _>>()
            .ok()
            .filter(|n| n.len() == 3)
            .with_context(|| format!("'{s}' is not a thread state. Copy it as printed."))?;
        Ok(Mark {
            items: n[0],
            lines: n[1],
            sessions: n[2],
        })
    }
}

/// `$AI_CHAT_DIR`, else `~/.ai/chat`: every repository's store, named by its root commit.
pub fn base_dir() -> Result<PathBuf> {
    store_dir("AI_CHAT_DIR", "chat")
}

/// The threads of one project: `<base>/<root commit>`, shared by its worktrees and clones.
pub struct Chat {
    repo: gix::Repository,
    id: String,
    root: PathBuf,
    cwd: PathBuf,
}

impl Chat {
    /// The threads of the repository at or above `dir`, in [`base_dir`]; `None` outside a
    /// repository or before its first commit. A store of another format is refused.
    pub fn find(dir: &Path) -> Result<Option<Chat>> {
        Chat::find_in(&base_dir()?, dir)
    }

    /// [`Chat::find`] with the stores in `base`.
    pub fn find_in(base: &Path, dir: &Path) -> Result<Option<Chat>> {
        let Some(Project { repo, id }) = Project::find(dir) else {
            return Ok(None);
        };
        let root = base.join(&id);
        file::check_format(&root, FORMAT, "chat")?;
        Ok(Some(Chat {
            repo,
            id,
            root,
            cwd: dir.to_path_buf(),
        }))
    }

    pub(super) fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    /// The directory it was found from.
    pub(super) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.root
    }

    /// Makes the store's directory and format file before its first write.
    fn ensure(root: &Path) -> Result<()> {
        file::ensure_format(root, FORMAT)
    }

    pub(super) fn session_lock(&self, harness: &str, session: &str) -> Result<file::Lock> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let dir = self.root.join("sessions").join(harness);
        fs::create_dir_all(&dir).at(&dir)?;
        file::lock(&dir.join(format!("{session}.lock")))
    }

    /// The attachment changes of every session, in order.
    fn events(&self) -> Result<Vec<Event>> {
        let p = self.root.join(SESSIONS);
        jsonl::decode_all(&jsonl::read(&p)?)
            .with_context(|| format!("The chat sessions file is corrupt: {}", file::pretty(&p)))
    }

    /// The thread `session` is attached to, from the last change of its attachment.
    pub(super) fn owner(&self, harness: &str, session: &str) -> Result<Option<String>> {
        let last = self
            .events()?
            .into_iter()
            .rfind(|e| e.harness == harness && e.session == session);
        match last.map(|e| e.state) {
            Some(State::Attached { thread }) => Ok(Some(check_name(&thread)?.into())),
            _ => Ok(None),
        }
    }

    /// Records an attachment change; the caller holds the session's lock.
    pub(super) fn record(&self, harness: &str, session: &str, state: State) -> Result<()> {
        Chat::ensure(&self.root)?;
        let _lock = file::lock(&self.root.join("sessions.lock"))?;
        let event = Event {
            harness: harness.into(),
            session: session.into(),
            at: Timestamp::now(),
            state,
        };
        jsonl::append(&self.root.join(SESSIONS), &[event])
    }

    /// The thread `name`; it exists once something is attached to it.
    pub fn thread(&self, name: &str) -> Result<Thread> {
        check_name(name)?;
        Ok(Thread {
            name: name.into(),
            dir: self.root.join(THREADS).join(name),
            root: self.root.clone(),
        })
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

    /// Whether `dir` lies in this project: an existing directory whose repository has this
    /// store's root commit, a worktree or clone of this one.
    pub fn holds(&self, dir: &Path) -> bool {
        Project::find(dir).is_some_and(|p| p.id == self.id)
    }
}

/// One thread: a directory `threads/<name>` of the store.
pub struct Thread {
    name: String,
    dir: PathBuf,
    root: PathBuf,
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
        let events = jsonl::read(&self.root.join(SESSIONS))?;
        let tracks = jsonl::read(&self.dir.join(SESSIONS))?;
        let lines = jsonl::read(&self.dir.join(LINES))?;
        let agents = jsonl::read(&self.dir.join(AGENTS))?;
        Ok(Snapshot {
            name: self.name.clone(),
            dir: self.dir.clone(),
            items: jsonl::read(&self.dir.join(ITEMS))?,
            lines,
            tracks,
            agents,
            events,
        })
    }

    /// The thread as it was at `mark`, one of its states.
    pub fn snapshot_at(&self, mark: Mark) -> Result<Snapshot> {
        let mut s = self.snapshot()?;
        for (b, n) in [
            (&mut s.items, mark.items),
            (&mut s.lines, mark.lines),
            (&mut s.tracks, mark.sessions),
        ] {
            let n = usize::try_from(n).unwrap_or(usize::MAX);
            if n > b.len() || (n > 0 && b[n - 1] != b'\n') {
                bail!("{mark} is not a state of thread {}.", self.name);
            }
            b.truncate(n);
        }
        Ok(s)
    }

    /// The writers' lock on this thread's files.
    pub(super) fn writer(&self) -> Result<file::Lock> {
        Chat::ensure(&self.root)?;
        file::lock(&self.root.join(format!("{}.write.lock", self.name)))
    }

    fn append<T: Serialize>(&self, file: &str, rows: &[T]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(&self.dir).at(&self.dir)?;
        jsonl::append(&self.dir.join(file), rows)
    }

    /// Records a session's branch; the caller holds the writer's lock.
    pub(super) fn put_track(&self, t: &Track) -> Result<()> {
        self.append(SESSIONS, std::slice::from_ref(t))
    }

    /// The compactor's lock on this thread; `None` while another process holds it.
    pub fn try_lock(&self) -> Result<Option<file::Lock>> {
        Chat::ensure(&self.root)?;
        file::try_lock(&self.lock_path())
    }

    /// The compactor's lock on this thread, waiting for it until `deadline`; `None` if another
    /// process still holds it then.
    pub fn lock_before(&self, deadline: Instant) -> Result<Option<file::Lock>> {
        Chat::ensure(&self.root)?;
        file::lock_before(&self.lock_path(), deadline)
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(format!("{}.lock", self.name))
    }

    /// The compactor's log, beside its lock; its directory is made if absent.
    pub fn log_path(&self) -> Result<PathBuf> {
        Chat::ensure(&self.root)?;
        Ok(self.root.join(format!("{}.log", self.name)))
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

    fn events(&self) -> Result<Vec<Event>> {
        jsonl::decode_all(&self.events).context("The chat sessions file is corrupt")
    }

    /// The binding of `t`, from its session's attachment changes `events`: when it was last
    /// attached here, and detached if a change followed. `None` until an attachment to this
    /// thread is recorded.
    fn bind(&self, t: Track, events: &[&Event]) -> Option<Binding> {
        let i = events.iter().rposition(
            |e| matches!(&e.state, State::Attached { thread } if *thread == self.name),
        )?;
        Some(Binding {
            attached: events[i].at,
            detached: events.get(i + 1).map(|e| e.at),
            harness: t.harness,
            session: t.session,
            transcript: t.transcript,
            graft: t.graft,
            tip: t.tip,
        })
    }

    /// Every session bound to this thread, attached or not, by harness and session id.
    pub fn bindings(&self) -> Result<Vec<Binding>> {
        let events = self.events()?;
        let mut by: HashMap<(&str, &str), Vec<&Event>> = HashMap::new();
        for e in &events {
            by.entry((&e.harness, &e.session)).or_default().push(e);
        }
        Ok(self
            .tracks()?
            .into_values()
            .filter_map(|t| {
                let events = by.get(&(t.harness.as_str(), t.session.as_str()))?.clone();
                self.bind(t, &events)
            })
            .collect())
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
        Ok(self.bind(t, &mine))
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

    /// Adds the items of `t` this thread lacks, for session `b`, and archives its files: a
    /// root hangs on `b.graft`, `b.tip` moves to its newest entry. Entries are known by
    /// harness and entry id. The caller holds `thread`'s writer lock and read this snapshot
    /// under it.
    pub(super) fn ingest(&self, thread: &Thread, b: &mut Track, t: &Transcript) -> Result<Added> {
        let sources = self
            .dir
            .join(SOURCES)
            .join(check_id(&b.harness, "harness")?)
            .join(check_id(&b.session, "session")?);
        for (relative, bytes) in &t.files {
            archive(&sources, relative, bytes)?;
        }
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
            if !items.rows.is_empty() {
                let dir = path.parent().expect("an agent file in a dir");
                fs::create_dir_all(dir).at(dir)?;
                jsonl::append(&path, &items.rows)?;
            }
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

/// Copies `bytes` to `relative` under `root`: appended when they extend the copy there, else
/// written whole, replacing a file or directory standing in the way.
fn archive(root: &Path, relative: &Path, bytes: &[u8]) -> Result<()> {
    let p = root.join(relative);
    let mut up: Vec<&Path> = relative
        .ancestors()
        .skip(1)
        .filter(|a| !a.as_os_str().is_empty())
        .collect();
    up.reverse();
    for a in up {
        let a = root.join(a);
        if fs::symlink_metadata(&a).is_ok_and(|m| !m.is_dir()) {
            fs::remove_file(&a).at(&a)?;
        }
    }
    match fs::symlink_metadata(&p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(&p).at(&p)?,
        Ok(_) => {
            let old = fs::read(&p).at(&p)?;
            if old == bytes {
                return Ok(());
            }
            if let Some(new) = bytes.strip_prefix(old.as_slice()) {
                return OpenOptions::new()
                    .append(true)
                    .open(&p)
                    .and_then(|mut f| f.write_all(new))
                    .at(&p);
            }
        }
        Err(_) => {}
    }
    let dir = p.parent().expect("a source file in a dir");
    fs::create_dir_all(dir).at(dir)?;
    fs::write(&p, bytes).at(&p)
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
    use crate::chat::model::turns;

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
