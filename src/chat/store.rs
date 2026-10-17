use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    ops::Range,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use gix::ObjectId;
use jiff::Timestamp;

use crate::{
    chat::{
        claude::Subagent,
        model::{Item, Kind, SourceId, Tree, turns},
        record::{Agent, Binding, Head, Key, Line, LineKind, check_id, decode, encode, item},
    },
    file::{self, AtPath},
    git::{Changes, Ref, Snap, fan_path},
    memory::Who,
};

pub const ME: &str = "ai chat";
/// Where the threads' refs live: `refs/ai/chat/<name>`.
pub const REFS: &str = "refs/ai/chat/";
/// The thread format this ai reads and writes.
pub const FORMAT: u32 = 2;
const SEG: u64 = 256;
const INDEX: &str = "index";
const ITEMS: &str = "items";
const RAW: &str = "raw";
const SESSIONS: &str = "sessions";
const AGENTS: &str = "agents";
const AGENT: &str = "agent";
const LINES: &str = "lines";

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

fn binding_path(harness: &str, session: &str) -> Result<String> {
    let (h, s) = (check_id(harness, "harness")?, check_id(session, "session")?);
    Ok(format!("{SESSIONS}/{h}/{s}"))
}

pub(super) fn put_binding(ch: &mut Changes, b: &Binding) -> Result<()> {
    ch.put(binding_path(&b.harness, &b.session)?, encode(b)?);
    Ok(())
}

/// Where a subagent's items live; the main tree's are at the root.
fn agent_dir(harness: &str, agent: &str) -> Result<String> {
    let (h, a) = (check_id(harness, "harness")?, check_id(agent, "agent")?);
    Ok(format!("{AGENTS}/{h}/{a}/"))
}

fn line_path(key: Key) -> String {
    fan_path(&format!("{LINES}/{}", key.level), key.last)
}

fn index_seg(b: &[u8]) -> Result<Vec<Head>> {
    if b.is_empty() {
        return Ok(Vec::new());
    }
    let heads: Vec<Head> = decode(b).context("An index segment is corrupt.")?;
    if heads.len() > SEG as usize {
        bail!("An index segment is corrupt: too many items.");
    }
    Ok(heads)
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

/// The threads of one git repository, in its common git dir, so its worktrees share them.
pub struct Chat {
    repo: gix::Repository,
}

impl Chat {
    /// The threads of the repository at or above `dir`; `None` outside one.
    pub fn find(dir: &Path) -> Result<Option<Chat>> {
        let Ok(mut repo) = gix::discover(dir) else {
            return Ok(None);
        };
        repo.committer_or_set_fallback(ME, "ai@localhost")?;
        Ok(Some(Chat { repo }))
    }

    pub(super) fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    pub(super) fn session_lock(&self, harness: &str, session: &str) -> Result<File> {
        check_id(harness, "harness")?;
        check_id(session, "session")?;
        let dir = self
            .repo
            .common_dir()
            .join("ai/chat/sessions")
            .join(harness);
        fs::create_dir_all(&dir).at(&dir)?;
        file::lock(&dir.join(format!("{session}.lock")))
    }

    /// The thread `name`; it exists once something is attached to it.
    pub fn thread(&self, name: &str) -> Result<Thread> {
        check_name(name)?;
        Ok(Thread {
            git: Ref::new(self.repo.clone(), format!("{REFS}{name}"), "thread", FORMAT),
            name: name.into(),
            lock: self
                .repo
                .common_dir()
                .join("ai/chat")
                .join(format!("{name}.lock")),
        })
    }

    /// Every thread, by name.
    pub fn threads(&self) -> Result<Vec<Thread>> {
        let mut names = Vec::new();
        for r in self.repo.references()?.prefixed(REFS)? {
            let r = r?;
            let full = r.name().as_bstr().to_string();
            if let Some(n) = full.strip_prefix(REFS).filter(|n| check_name(n).is_ok()) {
                names.push(n.to_string());
            }
        }
        names.sort();
        names.iter().map(|n| self.thread(n)).collect()
    }

    /// Every thread with its newest item, tips, turns and sessions.
    pub fn list(&self) -> Result<Vec<ThreadInfo>> {
        self.threads()?
            .iter()
            .map(|t| {
                let snap = t.snapshot()?;
                let heads = snap.heads()?;
                let tips = tips(&heads);
                let turns = match tips.last() {
                    Some(&tip) => turns(&snap.tree()?.path(tip as usize)).len(),
                    None => 0,
                };
                let mut sessions: Vec<(Binding, Timestamp)> = snap
                    .bindings()?
                    .into_iter()
                    .map(|b| {
                        let at = b.tip.map_or(b.attached, |t| heads[t as usize].ts);
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
            .collect()
    }

    /// Whether `dir` lies in this repository: an existing directory whose repository shares
    /// this one's common git dir.
    pub fn holds(&self, dir: &Path) -> bool {
        let canon = |d: &Path| fs::canonicalize(d).ok();
        let mine = canon(self.repo.common_dir());
        mine.is_some() && gix::discover(dir).is_ok_and(|r| canon(r.common_dir()) == mine)
    }
}

/// One thread: a ref `refs/ai/chat/<name>`.
pub struct Thread {
    git: Ref,
    name: String,
    lock: PathBuf,
}

impl Thread {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn exists(&self) -> Result<bool> {
        Ok(self.git.head()?.is_some())
    }

    pub(super) fn format(&self) -> Result<u32> {
        let format = self.git.raw(self.git.head()?)?.format()?;
        self.git.readable(format)?;
        Ok(format)
    }

    pub fn upgrade(&self) -> Result<bool> {
        let (upgraded, _) = self.git.mutate(
            "upgrade thread to format 2",
            |commit| self.git.raw(commit),
            |source| {
                let format = source.format()?;
                self.git.readable(format)?;
                if format == FORMAT {
                    return Ok((false, Changes::default()));
                }
                Ok((true, super::v1::export(source)?))
            },
        )?;
        Ok(upgraded)
    }

    /// The thread as of its latest commit; empty if it does not exist.
    pub fn snapshot(&self) -> Result<Snapshot<'_>> {
        self.at(self.git.head()?)
    }

    /// The thread as of `commit`, one of its commits.
    pub fn snapshot_at(&self, commit: ObjectId) -> Result<Snapshot<'_>> {
        self.at(Some(commit))
    }

    fn at(&self, commit: Option<ObjectId>) -> Result<Snapshot<'_>> {
        let git = self.git.raw(commit)?;
        let format = git.format()?;
        self.git.readable(format)?;
        if format < FORMAT {
            bail!(
                "This thread snapshot is format {format}; typed historical views require format {FORMAT}. Original v1 snapshots remain available through Git."
            );
        }
        Ok(Snapshot { git })
    }

    /// Applies `change` to the latest snapshot as one commit, redoing it until the ref update wins.
    pub(super) fn mutate<R>(
        &self,
        msg: &str,
        change: impl FnMut(&Snapshot) -> Result<(R, Changes)>,
    ) -> Result<(R, Snapshot<'_>)> {
        self.git.mutate(msg, |c| self.at(c), change)
    }

    /// The compactor's lock on this thread; `None` while another process holds it.
    pub fn try_lock(&self) -> Result<Option<File>> {
        self.dir()?;
        file::try_lock(&self.lock)
    }

    /// The compactor's lock on this thread, waiting for it until `deadline`; `None` if another
    /// process still holds it then.
    pub fn lock_before(&self, deadline: Instant) -> Result<Option<File>> {
        self.dir()?;
        file::lock_before(&self.lock, deadline)
    }

    /// The compactor's log, beside its lock; its directory is made if absent.
    pub fn log_path(&self) -> Result<PathBuf> {
        self.dir()?;
        Ok(self.lock.with_extension("log"))
    }

    fn dir(&self) -> Result<()> {
        let dir = self.lock.parent().expect("a lock in a dir");
        fs::create_dir_all(dir).at(dir)
    }

    /// Drops every line, in one commit; how many it dropped.
    pub fn drop_lines(&self) -> Result<usize> {
        let (n, _) = self.mutate("drop lines", |s| {
            let n = s.all_lines()?.len();
            let mut ch = Changes::default();
            if n > 0 {
                ch.remove(LINES.into());
            }
            Ok((n, ch))
        })?;
        Ok(n)
    }

    /// Drops the lines among `keys` that are built, in one commit; how many it dropped.
    pub fn drop_keys(&self, keys: &[Key]) -> Result<usize> {
        let (n, _) = self.mutate("drop lines", |s| {
            let mut ch = Changes::default();
            let mut n = 0;
            for &k in keys {
                if s.line(k)?.is_some() {
                    ch.remove(line_path(k));
                    n += 1;
                }
            }
            Ok((n, ch))
        })?;
        Ok(n)
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

    /// Writes the lines not built yet, in one commit; returns the line stored under each key.
    pub fn put_lines(&self, lines: Vec<(Key, Line)>) -> Result<Vec<Line>> {
        let msg = match &lines[..] {
            [(key, _)] => format!("line {key}"),
            _ => format!("{} lines", lines.len()),
        };
        let (stored, _) = self.mutate(&msg, |s| {
            let n = s.len()?;
            let mut ch = Changes::default();
            let mut stored = Vec::with_capacity(lines.len());
            for (key, line) in &lines {
                if let Some(l) = s.line(*key)? {
                    stored.push(l);
                    continue;
                }
                if key.last >= n {
                    bail!("Line {key} is beyond the thread: it holds {n} items.");
                }
                ch.put(line_path(*key), encode(line)?);
                stored.push(line.clone());
            }
            Ok((stored, ch))
        })?;
        Ok(stored)
    }
}

/// A thread as of one commit: every read through it sees the same state.
pub struct Snapshot<'t> {
    git: Snap<'t>,
}

impl<'t> AsRef<Snap<'t>> for Snapshot<'t> {
    fn as_ref(&self) -> &Snap<'t> {
        &self.git
    }
}

impl Snapshot<'_> {
    /// How many items the main tree holds.
    pub fn len(&self) -> Result<u64> {
        self.git.count(ITEMS)
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// The heads of the main tree's items, in order: position is sequence number.
    pub fn heads(&self) -> Result<Vec<Head>> {
        self.heads_in("")
    }

    fn heads_in(&self, dir: &str) -> Result<Vec<Head>> {
        let mut out: Vec<Head> = Vec::new();
        self.git.walk(&format!("{dir}{INDEX}"), |s, id| {
            let seg = index_seg(&self.git.fetch(id)?)?;
            for (k, head) in seg.into_iter().enumerate() {
                let seq = s * SEG + k as u64;
                if seq != out.len() as u64 {
                    bail!("Item {} is missing from the thread.", out.len());
                }
                if head.seq != seq || head.parent.is_some_and(|p| p >= seq) {
                    bail!("Item {seq} is corrupt.");
                }
                out.push(head);
            }
            Ok(())
        })?;
        if self.git.count(&format!("{dir}{ITEMS}"))? != out.len() as u64 {
            bail!("The thread's index and item sequence have different lengths.");
        }
        Ok(out)
    }

    /// The items of the main tree, in order: position is sequence number.
    pub fn items(&self) -> Result<Vec<Item>> {
        self.items_in("")
    }

    fn items_in(&self, dir: &str) -> Result<Vec<Item>> {
        let heads = self.heads_in(dir)?;
        let blobs = |sub: &str| -> Result<Vec<ObjectId>> {
            let mut ids = Vec::with_capacity(heads.len());
            self.git.walk(&format!("{dir}{sub}"), |i, id| {
                if i != ids.len() as u64 {
                    bail!("Item {} is missing from the thread.", ids.len());
                }
                ids.push(id);
                Ok(())
            })?;
            if ids.len() != heads.len() {
                bail!("The thread's index and {sub} sequence have different lengths.");
            }
            Ok(ids)
        };
        let (kinds, raws) = (blobs(ITEMS)?, blobs(RAW)?);
        heads
            .iter()
            .map(|h| {
                let i = h.seq as usize;
                let (Some(&k), Some(&r)) = (kinds.get(i), raws.get(i)) else {
                    bail!("Item {i} is missing from the thread.");
                };
                let parent = h.parent.map(|p| heads[p as usize].source());
                item(h, parent, &self.git.fetch(k)?, &self.git.fetch(r)?)
                    .with_context(|| format!("Item {i} is corrupt"))
            })
            .collect()
    }

    /// Item `seq` of the main tree.
    pub fn item(&self, seq: u64) -> Result<Item> {
        let head = |seq: u64| -> Result<Head> {
            let seg = index_seg(&self.git.read(&fan_path(INDEX, seq / SEG))?)?;
            let head = seg
                .get((seq % SEG) as usize)
                .with_context(|| format!("No item {seq}."))?;
            if head.seq != seq || head.parent.is_some_and(|p| p >= seq) {
                bail!("Item {seq} is corrupt.");
            }
            Ok(head.clone())
        };
        let h = head(seq)?;
        let parent = h.parent.map(|p| head(p).map(|p| p.source())).transpose()?;
        let blob = |dir: &str| -> Result<Vec<u8>> {
            let id = self
                .git
                .entry(&fan_path(dir, seq))?
                .with_context(|| format!("Item {seq} is missing from the thread."))?;
            self.git.fetch(id)
        };
        item(&h, parent, &blob(ITEMS)?, &blob(RAW)?)
            .with_context(|| format!("Item {seq} is corrupt"))
    }

    /// The main tree: positions are sequence numbers.
    pub fn tree(&self) -> Result<Tree> {
        Ok(Tree::new(self.items()?))
    }

    /// The items of the main tree nothing follows, in order: the last is the newest item.
    pub fn tips(&self) -> Result<Vec<u64>> {
        Ok(tips(&self.heads()?))
    }

    /// Every session bound to this thread, attached or not, by harness and session id.
    pub fn bindings(&self) -> Result<Vec<Binding>> {
        let mut out = Vec::new();
        for (harness, _) in self.git.names(SESSIONS)? {
            for (session, id) in self.git.names(&format!("{SESSIONS}/{harness}"))? {
                let b: Binding = decode(&self.git.fetch(id)?)
                    .with_context(|| format!("The binding of {harness} {session} is corrupt"))?;
                if b.harness != harness || b.session != session {
                    bail!("The binding of {harness} {session} has a different identity.");
                }
                out.push(b);
            }
        }
        Ok(out)
    }

    pub fn binding(&self, harness: &str, session: &str) -> Result<Option<Binding>> {
        let b = self.git.read(&binding_path(harness, session)?)?;
        if b.is_empty() {
            return Ok(None);
        }
        let binding: Binding =
            decode(&b).with_context(|| format!("The binding of {harness} {session} is corrupt"))?;
        if binding.harness != harness || binding.session != session {
            bail!("The binding of {harness} {session} has a different identity.");
        }
        Ok(Some(binding))
    }

    /// The newest tip of a session: the item of its transcript's newest entry.
    pub fn tip(&self, harness: &str, session: &str) -> Result<Option<u64>> {
        Ok(self.binding(harness, session)?.and_then(|b| b.tip))
    }

    /// Every subagent, by harness and agent id.
    pub fn agents(&self) -> Result<Vec<Agent>> {
        let mut out = Vec::new();
        for (harness, _) in self.git.names(AGENTS)? {
            for (agent, _) in self.git.names(&format!("{AGENTS}/{harness}"))? {
                out.extend(self.agent(&harness, &agent)?);
            }
        }
        Ok(out)
    }

    pub fn agent(&self, harness: &str, agent: &str) -> Result<Option<Agent>> {
        let b = self
            .git
            .read(&format!("{}{AGENT}", agent_dir(harness, agent)?))?;
        if b.is_empty() {
            return Ok(None);
        }
        let record: Agent =
            decode(&b).with_context(|| format!("The record of agent {agent} is corrupt"))?;
        if record.harness != harness || record.agent != agent {
            bail!("The record of agent {agent} has a different identity.");
        }
        Ok(Some(record))
    }

    /// A subagent's items, in order; its roots follow its [`Agent::call`].
    pub fn agent_items(&self, harness: &str, agent: &str) -> Result<Vec<Item>> {
        self.items_in(&agent_dir(harness, agent)?)
    }

    pub fn line(&self, key: Key) -> Result<Option<Line>> {
        let b = self.git.read(&line_path(key))?;
        if b.is_empty() {
            return Ok(None);
        }
        decode(&b)
            .map(Some)
            .with_context(|| format!("Line {key} is corrupt"))
    }

    /// Every line, by key.
    pub fn all_lines(&self) -> Result<BTreeMap<Key, Line>> {
        let mut out = BTreeMap::new();
        for (name, _) in self.git.names(LINES)? {
            let level = name
                .parse()
                .with_context(|| format!("A line level is misnamed: {name}."))?;
            for (last, line) in self.lines(level)? {
                out.insert(Key { level, last }, line);
            }
        }
        Ok(out)
    }

    /// The commit this snapshot reads; none for a thread that does not exist.
    pub fn commit(&self) -> Option<ObjectId> {
        self.git.commit()
    }

    /// Every line of `level`, by the last item it covers.
    pub fn lines(&self, level: u32) -> Result<Vec<(u64, Line)>> {
        let mut out = Vec::new();
        self.git.walk(&format!("{LINES}/{level}"), |last, id| {
            let line = decode(&self.git.fetch(id)?)
                .with_context(|| format!("Line {} is corrupt", Key { level, last }))?;
            out.push((last, line));
            Ok(())
        })?;
        Ok(out)
    }

    /// Adds the items of `t` this thread lacks, for session `b`: a root hangs on `b.graft`,
    /// `b.tip` moves to its newest entry. Entries are known by harness and entry id.
    pub(super) fn ingest(
        &self,
        b: &mut Binding,
        t: &Transcript,
        ch: &mut Changes,
    ) -> Result<Added> {
        for (relative, bytes) in &t.files {
            let relative = relative
                .to_str()
                .context("A source file name is not UTF-8.")?;
            let path = format!("sources/{}/{}/{relative}", b.harness, b.session);
            if self.git.entry(&path)?.is_none() || &*self.git.read(&path)? != bytes {
                ch.put(path, bytes.clone());
            }
        }
        let mut main = Append::new(self, String::new())?;
        let first = main.len;
        for item in &t.items {
            if main.seq(&item.id).is_none() {
                let parent = item.parent.as_ref().and_then(|p| main.seq(p));
                main.push(item, parent.or(b.graft), ch)?;
            }
        }
        if let Some(last) = t.items.last() {
            b.tip = main.seq(&last.id);
        }
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
            agent_items += self.ingest_agent(&b.harness, sub, call, ch)?;
        }
        let added = first..main.len;
        main.finish(ch)?;
        Ok(Added {
            items: added,
            agent_items,
        })
    }

    fn ingest_agent(
        &self,
        harness: &str,
        sub: &Subagent,
        call: Option<u64>,
        ch: &mut Changes,
    ) -> Result<u64> {
        let dir = agent_dir(harness, &sub.agent)?;
        let mut items = Append::new(self, dir.clone())?;
        let first = items.len;
        for item in &sub.items {
            if items.seq(&item.id).is_none() {
                let parent = item.parent.as_ref().and_then(|p| items.seq(p));
                items.push(item, parent, ch)?;
            }
        }
        let previous = self.agent(harness, &sub.agent)?;
        let agent = Agent {
            harness: harness.into(),
            agent: sub.agent.clone(),
            agent_type: sub.agent_type.clone(),
            description: sub.description.clone(),
            call: call.or_else(|| previous.as_ref().and_then(|agent| agent.call)),
        };
        if previous.as_ref() != Some(&agent) {
            ch.put(format!("{dir}{AGENT}"), encode(&agent)?);
        }
        let added = items.len - first;
        items.finish(ch)?;
        Ok(added)
    }
}

/// New items of one item sequence (the main tree's, or a subagent's) in one mutation.
struct Append<'a, 't> {
    snap: &'a Snapshot<'t>,
    dir: String,
    len: u64,
    keys: HashMap<(String, String), u64>,
    segs: BTreeMap<u64, Vec<Head>>,
}

impl<'a, 't> Append<'a, 't> {
    fn new(snap: &'a Snapshot<'t>, dir: String) -> Result<Self> {
        let heads = snap.heads_in(&dir)?;
        let keys = heads
            .iter()
            .map(|h| ((h.harness.clone(), h.entry.clone()), h.seq))
            .collect();
        Ok(Append {
            snap,
            len: heads.len() as u64,
            dir,
            keys,
            segs: BTreeMap::new(),
        })
    }

    /// The number of the item from `id`'s entry, if the sequence holds it.
    fn seq(&self, id: &SourceId) -> Option<u64> {
        self.keys.get(&key(id)).copied()
    }

    fn push(&mut self, item: &Item, parent: Option<u64>, ch: &mut Changes) -> Result<()> {
        let seq = self.len;
        let s = seq / SEG;
        let index = format!("{}{INDEX}", self.dir);
        if !self.segs.contains_key(&s) {
            let seg = index_seg(&self.snap.git.read(&fan_path(&index, s))?)?;
            self.segs.insert(s, seg);
        }
        let seg = self.segs.get_mut(&s).expect("inserted above");
        if seg.len() as u64 != seq % SEG {
            bail!(
                "Item {seq} is corrupt: its index segment holds {} items.",
                seg.len()
            );
        }
        let head = Head {
            seq,
            parent,
            harness: item.id.harness.clone(),
            session: item.id.session.clone(),
            entry: item.id.entry.clone(),
            ts: item.ts,
        };
        seg.push(head);
        ch.put(
            fan_path(&format!("{}{ITEMS}", self.dir), seq),
            encode(&item.kind)?,
        );
        ch.put(
            fan_path(&format!("{}{RAW}", self.dir), seq),
            item.raw.clone().into_bytes(),
        );
        self.keys.insert(key(&item.id), seq);
        self.len += 1;
        Ok(())
    }

    fn finish(self, ch: &mut Changes) -> Result<()> {
        let index = format!("{}{INDEX}", self.dir);
        for (s, seg) in self.segs {
            ch.put(fan_path(&index, s), encode(&seg)?);
        }
        Ok(())
    }
}
