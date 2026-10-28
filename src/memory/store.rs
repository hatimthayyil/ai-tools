use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, anyhow, bail};
use gix::ObjectId;
use tempfile::NamedTempFile;

use crate::{
    file::{self, AtPath, pretty},
    git::{Changes, Ref, Snap, fan_path},
    memory::{
        meta::stamp,
        record::{COLUMNS, Message, Node, Who, now},
        tree::{Coord, LEVELS, free, joined},
        upgrade::{self, FORMAT},
    },
    record::Seg,
};

pub const ME: &str = "ai memory";
pub const REF: &str = "refs/ai/memory";
pub const LOG: &str = "log";
const SEG: u64 = 256;
const ORIGIN: &str = "origin";
/// The compactor's lock file; an upgrade takes it too.
const LOCK: &str = "compact.lock";
/// Every tree of nodes, each under its number.
const TREES: &str = "trees";
/// Which tree is current, which is being built, and the highest number given to one.
pub const STATE: &str = "state";

/// The directory of tree `tree`'s nodes at level `l`.
pub fn level_dir(tree: u64, l: u32) -> String {
    format!("{TREES}/{tree}/{l}")
}

fn seg_of(tree: u64, c: Coord) -> (String, usize) {
    (
        fan_path(&level_dir(tree, c.l), c.i / SEG),
        (c.i % SEG) as usize,
    )
}

/// Nodes a mutation wrote, with their stored text.
pub type Built = Vec<(Coord, String)>;

/// The trees of this memory, by number. Trees are copies of one another until a rebuild writes
/// a summary into one; then the others stay as they were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub current: u64,
    /// The tree a rebuild is writing into, if one is.
    pub building: Option<u64>,
    /// The highest number ever given to a tree; numbers are not reused.
    pub last: u64,
}

impl Default for State {
    fn default() -> State {
        State {
            current: 1,
            building: None,
            last: 1,
        }
    }
}

impl State {
    fn parse(b: &[u8]) -> Result<State> {
        let corrupt = || anyhow!("The memory's state is corrupt.");
        let s = std::str::from_utf8(b).map_err(|_| corrupt())?;
        let mut st = State::default();
        for line in s.lines() {
            let (k, v) = line.split_once(' ').ok_or_else(corrupt)?;
            let v: u64 = v.parse().map_err(|_| corrupt())?;
            match k {
                "current" => st.current = v,
                "building" => st.building = Some(v),
                "last" => st.last = v,
                _ => return Err(corrupt()),
            }
        }
        Ok(st)
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut s = format!("current {}\n", self.current);
        if let Some(b) = self.building {
            s += &format!("building {b}\n");
        }
        s += &format!("last {}\n", self.last);
        s.into_bytes()
    }

    /// The tree writes go to: the one being built, else the current one.
    pub fn working(&self) -> u64 {
        self.building.unwrap_or(self.current)
    }
}

/// `state` as the only change of a mutation.
fn saved(st: State) -> Changes {
    let mut ch = Changes::default();
    ch.put(STATE.into(), st.bytes());
    ch
}

pub struct Store {
    git: Ref,
    dir: PathBuf,
    origin: String,
}

/// The origin in `p`, `None` without the file.
fn read_origin(p: &Path) -> Result<Option<String>> {
    let o = match fs::read_to_string(p) {
        Ok(o) => o,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).at(p),
    };
    match o.trim() {
        o if !o.is_empty() && !o.contains(char::is_whitespace) => Ok(Some(o.into())),
        _ => bail!("{} is not an origin: one word.", pretty(p)),
    }
}

/// `len` Crockford base-32 digits of `bits`.
fn crockford(bits: u32, len: usize) -> String {
    const ABC: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    (0..len)
        .map(|i| ABC[(bits >> (5 * i) & 31) as usize] as char)
        .collect()
}

impl Store {
    /// Opens an existing memory; only [`Store::create`] makes one.
    pub fn open(dir: &Path) -> Result<Store> {
        match Store::find(dir)? {
            Some(s) => Ok(s),
            None => bail!(
                "No memory at {}.\nTo create one, run: {ME} init\n\
                 To use an existing one, point AI_MEMORY_DIR at it.",
                dir.display()
            ),
        }
    }

    /// Opens the memory at `dir`, creating it if absent; returns whether it was new.
    pub fn create(dir: &Path) -> Result<(Store, bool)> {
        if let Some(s) = Store::find(dir)? {
            return Ok((s, false));
        }
        if fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()) {
            bail!(
                "{} is not a memory, and not empty. Point AI_MEMORY_DIR elsewhere.",
                pretty(dir)
            );
        }
        fs::create_dir_all(dir).at(dir)?;
        gix::init_bare(dir)?;
        Ok((Store::open(dir)?, true))
    }

    fn find(dir: &Path) -> Result<Option<Store>> {
        if !dir.is_dir() {
            return Ok(None);
        }
        let Ok(mut repo) = gix::open(dir) else {
            return Ok(None);
        };
        if !repo.is_bare() {
            return Ok(None);
        }
        repo.committer_or_set_fallback(ME, "ai@localhost")?;
        let mut s = Store {
            git: Ref::new(repo, REF.into(), "memory", FORMAT),
            dir: dir.to_path_buf(),
            origin: String::new(),
        };
        s.upgrade()?;
        s.origin = s.load_origin()?;
        Ok(Some(s))
    }

    /// Brings the memory to [`FORMAT`] in one commit on its head; refuses a newer one.
    fn upgrade(&self) -> Result<()> {
        let v = self.git.raw(self.head()?)?.format()?;
        self.git.readable(v)?;
        if v == FORMAT {
            return Ok(());
        }
        let _lock = match self.try_lock()? {
            Some(l) => l,
            None => {
                eprintln!("Waiting for the running summariser to finish, to upgrade this memory.");
                file::lock(&self.dir.join(LOCK))?
            }
        };
        loop {
            let head = self.head()?;
            let base = self.git.raw(head)?;
            let from = base.format()?;
            self.git.readable(from)?;
            if from == FORMAT {
                return Ok(());
            }
            let mut tree = base.tree_id();
            for v in from..FORMAT {
                let s = self.snap_of(self.git.tree(head, tree))?;
                tree = self.git.write_tree(tree, upgrade::step(&s, v)?)?;
            }
            let msg = format!("upgrade format {from} -> {FORMAT}");
            let commit = self.git.new_commit(&msg, tree, head)?;
            if self.git.cas(head, commit, &msg)? {
                let prev = head.map_or_else(String::new, |h| h.to_string());
                eprintln!("Upgraded memory to format {FORMAT}; previous state is commit {prev}.");
                return Ok(());
            }
        }
    }

    /// The compactor's lock on this memory; `None` while another process holds it.
    pub fn try_lock(&self) -> Result<Option<file::Lock>> {
        file::try_lock(&self.dir.join(LOCK))
    }

    /// The origin in this store's `origin` file, made by the first open.
    fn load_origin(&self) -> Result<String> {
        let p = self.dir.join(ORIGIN);
        if let Some(o) = read_origin(&p)? {
            return Ok(o);
        }
        let mut f = NamedTempFile::new_in(&self.dir).at(&self.dir)?;
        writeln!(f, "{}", crockford(getrandom::u32()?, 6)).at(f.path())?;
        match f.persist_noclobber(&p) {
            Ok(_) => {}
            Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.error).at(&p),
        }
        read_origin(&p)?.with_context(|| format!("{} vanished.", pretty(&p)))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// This store's id, stamped on every message and node it writes.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Replaces this store's id in its `origin` file.
    pub fn set_origin(&mut self, origin: &str) -> Result<()> {
        if origin.is_empty() || origin.contains(char::is_whitespace) {
            bail!("'{origin}' is not an origin: one word.");
        }
        let p = self.dir.join(ORIGIN);
        let mut f = NamedTempFile::new_in(&self.dir).at(&self.dir)?;
        writeln!(f, "{origin}").at(f.path())?;
        f.persist(&p).map_err(|e| e.error).at(&p)?;
        self.origin = origin.into();
        Ok(())
    }

    pub fn head(&self) -> Result<Option<ObjectId>> {
        self.git.head()
    }

    /// The current tree of the memory as of the latest commit.
    pub fn snapshot(&self) -> Result<Snapshot<'_>> {
        let s = self.at(self.head()?)?;
        let current = s.state.current;
        Ok(s.reading(current))
    }

    /// The memory as of the latest commit, reading and writing the tree being built if there is
    /// one, else the current tree.
    pub fn working(&self) -> Result<Snapshot<'_>> {
        self.at(self.head()?)
    }

    /// The memory as of `commit`, which must be of [`FORMAT`]; `None` is the empty memory.
    fn at(&self, commit: Option<ObjectId>) -> Result<Snapshot<'_>> {
        let git = self.git.raw(commit)?;
        let v = git.format()?;
        self.git.readable(v)?;
        if v < FORMAT {
            bail!(
                "This memory is format {v}, older than this ai's {FORMAT}. Run ai view to upgrade it."
            );
        }
        self.snap_of(git)
    }

    /// `git`, with its state read and its working tree selected.
    fn snap_of<'s>(&'s self, git: Snap<'s>) -> Result<Snapshot<'s>> {
        let state = State::parse(&git.read(STATE)?)?;
        Ok(Snapshot {
            store: self,
            git,
            tree: state.working(),
            state,
        })
    }

    /// Applies `change` to the latest snapshot as one commit, redoing it until the ref update wins.
    pub fn mutate<R>(
        &self,
        msg: &str,
        change: impl FnMut(&Snapshot) -> Result<(R, Changes)>,
    ) -> Result<(R, Snapshot<'_>)> {
        self.git.mutate(msg, |c| self.at(c), change)
    }

    /// Appends `items` in order, with every free node they complete, in the tree being built
    /// and the current one; returns the first new id. A message without an origin gets this
    /// store's, one without a ts the time it lands, never before the last message's: the log is
    /// in time order. Every other field is kept as given.
    pub fn append(&self, msg: &str, items: &[Message]) -> Result<(u64, Built, Snapshot<'_>)> {
        let ((first, built), snap) = self.mutate(msg, |s| {
            let mut ed = Edit::new(s, s.mirrors())?;
            let first = ed.t;
            for m in items {
                ed.push(m)?;
            }
            let (built, ch) = ed.finish();
            Ok(((first, built), ch))
        })?;
        Ok((first, built, snap))
    }

    /// Writes node `c` in the tree being built, unless it is fresh there, with every free node
    /// above it it completes.
    pub fn put_node(&self, c: Coord, who: &Who, text: &str) -> Result<(Built, Snapshot<'_>)> {
        self.mutate(&format!("node {c}"), |s| {
            let tree = s.tree;
            let mut ed = Edit::new(s, vec![tree])?;
            if c.end() > ed.t {
                bail!("Node {c} is beyond the log: it holds {} messages.", ed.t);
            }
            if !ed.fresh(tree, c)? {
                ed.put(tree, c, who, text)?;
            }
            Ok(ed.finish())
        })
    }

    /// Makes a copy of tree `from` as the next tree, which is then being built, and writes into
    /// it the free nodes that the messages make. Returns its number.
    pub fn start_building(&self, from: u64) -> Result<u64> {
        let b = self.copy_building(from)?;
        self.complete_building()?;
        Ok(b)
    }

    /// The copy of [`Store::start_building`], without the free nodes: a crash after it leaves
    /// them to [`Store::complete_building`]. Returns the number of the copy.
    pub fn copy_building(&self, from: u64) -> Result<u64> {
        let (b, _) = self.mutate("tree: copy", |s| {
            if s.state.building.is_some() {
                bail!("A tree is already being built.");
            }
            let n = s.state.last + 1;
            let mut ch = Changes::default();
            for l in 0..LEVELS {
                s.walk(&level_dir(from, l), |k, id| {
                    ch.put(fan_path(&level_dir(n, l), k), s.fetch(id)?);
                    Ok(())
                })?;
            }
            let st = State {
                building: Some(n),
                last: n,
                ..s.state
            };
            ch.put(STATE.into(), st.bytes());
            Ok((n, ch))
        })?;
        Ok(b)
    }

    /// Writes into the tree being built every free node that the messages make and it lacks,
    /// which a crash in [`Store::copy_building`] may have left out. Nothing without a tree being
    /// built, and no commit when nothing is missing.
    pub fn complete_building(&self) -> Result<()> {
        match self.snapshot()?.state().building {
            Some(b) => self.catch_up(b),
            None => Ok(()),
        }
    }

    /// Writes into `tree` every free node that the messages make and it lacks.
    fn catch_up(&self, tree: u64) -> Result<()> {
        self.mutate("tree: free nodes", |s| {
            let mut ed = Edit::new(s, vec![tree])?;
            let end = ed.t;
            s.scan(end, |i, m| ed.leaf(i, &m))?;
            Ok(((), ed.finish().1))
        })?;
        Ok(())
    }

    /// Marks each of `nodes` stale in the tree being built, skipping those it lacks; how many it
    /// marked. A stale node keeps its text until a build rewrites it.
    pub fn mark_stale(&self, nodes: &[Coord]) -> Result<u64> {
        let (n, _) = self.mutate("tree: stale", |s| {
            if s.state.building != Some(s.tree) {
                bail!("No tree is being built.");
            }
            let mut ed = Edit::new(s, vec![s.tree])?;
            let mut n = 0;
            for &c in nodes {
                if ed.mark(s.tree, c)? {
                    n += 1;
                }
            }
            Ok((n, ed.finish().1))
        })?;
        Ok(n)
    }

    /// Makes tree `to` the current tree, stale or not. A tree being built stays as it is, unless
    /// it is `to`: then it is current, and no longer being built.
    pub fn switch(&self, to: u64) -> Result<()> {
        self.mutate("tree: use", |s| {
            let st = State {
                current: to,
                building: s.state.building.filter(|&b| b != to),
                ..s.state
            };
            Ok(((), saved(st)))
        })?;
        Ok(())
    }

    /// Drops tree `n`, which is not the current one; dropping the tree being built cancels it.
    pub fn drop_tree(&self, n: u64) -> Result<()> {
        self.mutate("tree: drop", |s| {
            let mut st = s.state;
            if st.current == n {
                bail!("Tree {n} is current.");
            }
            if st.building == Some(n) {
                st.building = None;
            }
            let mut ch = saved(st);
            ch.remove(format!("{TREES}/{n}"));
            Ok(((), ch))
        })?;
        Ok(())
    }
}

/// One mutation: new messages and nodes over a snapshot, free nodes completed as they become ready
/// in each tree it writes.
struct Edit<'a, 's> {
    snap: &'a Snapshot<'s>,
    /// The trees it writes. The first is the one `built` reports.
    trees: Vec<u64>,
    t: u64,
    ch: Changes,
    segs: BTreeMap<String, Seg>,
    built: Built,
    last: Option<String>,
}

impl<'a, 's> Edit<'a, 's> {
    fn new(snap: &'a Snapshot<'s>, trees: Vec<u64>) -> Result<Self> {
        Ok(Edit {
            snap,
            trees,
            t: snap.log_len()?,
            ch: Changes::default(),
            segs: BTreeMap::new(),
            built: Vec::new(),
            last: None,
        })
    }

    fn seg(&mut self, path: &str) -> Result<&mut Seg> {
        if !self.segs.contains_key(path) {
            let stored = self.snap.read(path)?;
            let seg = seg(&stored)?;
            self.segs.insert(path.into(), seg);
        }
        Ok(self.segs.get_mut(path).expect("inserted above"))
    }

    fn node(&mut self, tree: u64, c: Coord) -> Result<Option<Node>> {
        let (path, k) = seg_of(tree, c);
        Ok(node(self.seg(&path)?, k))
    }

    /// Whether `c` is built in `tree` and not stale.
    fn fresh(&mut self, tree: u64, c: Coord) -> Result<bool> {
        Ok(self.node(tree, c)?.is_some_and(|n| !n.stale))
    }

    /// Stores `node` as `c` in `tree`; returns its text as it reads back.
    fn write(&mut self, tree: u64, c: Coord, node: &Node) -> Result<String> {
        let (path, k) = seg_of(tree, c);
        let seg = self.seg(&path)?;
        let line = node.encode(&seg.cols);
        let text = Node::decode(&line, &seg.cols)
            .context("An empty node.")?
            .text;
        if seg.lines.len() <= k {
            seg.lines.resize(k + 1, String::new());
        }
        seg.lines[k] = line;
        Ok(text)
    }

    fn put(&mut self, tree: u64, c: Coord, who: &Who, text: &str) -> Result<()> {
        let node = Node {
            ts: now(),
            origin: self.snap.store.origin.clone(),
            who: who.clone(),
            stale: false,
            text: text.into(),
        };
        let text = self.write(tree, c, &node)?;
        if tree == self.trees[0] {
            self.built.push((c, text));
        }
        self.cascade(tree, c)
    }

    /// Marks node `c` stale in `tree` if it is built there; whether it is. A note kept as its own
    /// text has no summary to rebuild, so it is not marked.
    fn mark(&mut self, tree: u64, c: Coord) -> Result<bool> {
        if c.l == 0 && free(self.snap.message(c.i)?.line()).is_some() {
            return Ok(false);
        }
        let Some(mut n) = self.node(tree, c)? else {
            return Ok(false);
        };
        n.stale = true;
        self.write(tree, c, &n)?;
        Ok(true)
    }

    /// Writes the parent of `c` in `tree` if it is missing and both its children are fresh there.
    fn cascade(&mut self, tree: u64, c: Coord) -> Result<()> {
        let p = c.parent();
        if p.end() > self.t || self.node(tree, p)?.is_some() {
            return Ok(());
        }
        let [a, b] = p.children();
        let (Some(a), Some(b)) = (self.fresh_node(tree, a)?, self.fresh_node(tree, b)?) else {
            return Ok(());
        };
        if a.text.is_empty() || b.text.is_empty() {
            return self.put(tree, p, &Who::default(), &joined(&a.text, &b.text));
        }
        match free(joined(&a.text, &b.text)) {
            Some(text) => self.put(tree, p, &Who::default(), &text),
            None => Ok(()),
        }
    }

    fn fresh_node(&mut self, tree: u64, c: Coord) -> Result<Option<Node>> {
        Ok(self.node(tree, c)?.filter(|n| !n.stale))
    }

    fn push(&mut self, m: &Message) -> Result<()> {
        if m.text.is_empty() {
            bail!("An empty message.");
        }
        let i = self.t;
        let mut m = m.clone();
        if m.origin.is_empty() {
            m.origin.clone_from(&self.snap.store.origin);
        }
        let last = match self.last.take() {
            Some(ts) => ts,
            None if i == 0 => String::new(),
            None => self.snap.message(i - 1)?.ts,
        };
        if m.ts.is_empty() {
            m.ts = now().max(last);
        } else if m.ts < last {
            bail!(
                "A message of {} cannot follow one of {} (UTC): the log is in time order.",
                stamp(&m.ts),
                stamp(&last)
            );
        }
        self.last = Some(m.ts.clone());
        self.ch.put(fan_path(LOG, i), m.encode());
        self.t += 1;
        self.leaf(i, &m)
    }

    /// Writes leaf `i`, of message `m`, into each tree it lacks, if the message is free.
    fn leaf(&mut self, i: u64, m: &Message) -> Result<()> {
        let Some(text) = free(m.line()) else {
            return Ok(());
        };
        for tree in self.trees.clone() {
            if !self.fresh(tree, Coord::leaf(i))? {
                self.put(tree, Coord::leaf(i), &Who::default(), &text)?;
            }
        }
        Ok(())
    }

    fn finish(mut self) -> (Built, Changes) {
        for (path, seg) in self.segs {
            if !seg.lines.is_empty() {
                self.ch.put(path, seg.bytes());
            }
        }
        (self.built, self.ch)
    }
}

/// The node segment stored as `b`: a column row ending in `text` and holding `ts`, then one
/// line per node, empty where not built. None is an empty one under [`COLUMNS`].
pub fn seg(b: &[u8]) -> Result<Seg> {
    let corrupt = || anyhow!("A tree segment is corrupt.");
    let seg = Seg::parse(b, &COLUMNS).ok_or_else(corrupt)?;
    if seg.cols.last().is_none_or(|c| c != "text") || !seg.cols.iter().any(|c| c == "ts") {
        return Err(corrupt());
    }
    Ok(seg)
}

fn node(seg: &Seg, k: usize) -> Option<Node> {
    Node::decode(seg.lines.get(k)?, &seg.cols)
}

/// The memory as of one commit, reading the tree `tree` of it: every read through it sees the
/// same state.
pub struct Snapshot<'s> {
    store: &'s Store,
    git: Snap<'s>,
    tree: u64,
    state: State,
}

impl<'s> AsRef<Snap<'s>> for Snapshot<'s> {
    fn as_ref(&self) -> &Snap<'s> {
        &self.git
    }
}

impl<'s> Snapshot<'s> {
    /// This snapshot, reading tree `tree`.
    fn reading(mut self, tree: u64) -> Self {
        self.tree = tree;
        self
    }

    /// This snapshot, reading tree `n`, which must exist.
    pub fn tree(mut self, n: u64) -> Result<Self> {
        if n == 0 || n > self.state.last || (n != self.state.current && !self.has_tree(n)?) {
            bail!("No tree {n}.");
        }
        self.tree = n;
        Ok(self)
    }

    /// The tree this snapshot reads.
    pub fn number(&self) -> u64 {
        self.tree
    }

    /// Which tree is current, which is being built, and the last number given.
    pub fn state(&self) -> State {
        self.state
    }

    /// Whether tree `n` is stored, not dropped.
    pub fn has_tree(&self, n: u64) -> Result<bool> {
        Ok(self.git.entry(&format!("{TREES}/{n}"))?.is_some())
    }

    /// The trees a write through this snapshot changes: the tree being built, and the current
    /// one.
    fn mirrors(&self) -> Vec<u64> {
        let mut trees = vec![self.tree];
        if self.state.current != self.tree {
            trees.push(self.state.current);
        }
        trees
    }

    pub(crate) fn fetch(&self, id: ObjectId) -> Result<Vec<u8>> {
        self.git.fetch(id)
    }

    /// The blob at `path`, empty if absent; cached.
    pub fn read(&self, path: &str) -> Result<Rc<[u8]>> {
        self.git.read(path)
    }

    /// Every blob under the fanned-out `dir`, with its number.
    pub(crate) fn walk(
        &self,
        dir: &str,
        each: impl FnMut(u64, ObjectId) -> Result<()>,
    ) -> Result<()> {
        self.git.walk(dir, each)
    }

    /// How many messages the log holds.
    pub fn log_len(&self) -> Result<u64> {
        self.git.count(LOG)
    }

    pub fn message(&self, i: u64) -> Result<Message> {
        let id = self
            .git
            .entry(&fan_path(LOG, i))?
            .with_context(|| format!("Message {i} is missing from the log."))?;
        Message::decode(&self.fetch(id)?).with_context(|| format!("Message {i}"))
    }

    /// Streams messages `[0, end)` in order.
    pub fn scan(&self, end: u64, mut each: impl FnMut(u64, Message) -> Result<()>) -> Result<()> {
        let mut want = 0;
        self.walk(LOG, |i, id| {
            if i >= end {
                return Ok(());
            }
            if i != want {
                bail!("Message {want} is missing from the log.");
            }
            want += 1;
            each(i, Message::decode(&self.fetch(id)?)?)
        })
    }

    /// Node `c` of the tree this snapshot reads, if built.
    pub fn node(&self, c: Coord) -> Result<Option<Node>> {
        let (path, k) = seg_of(self.tree, c);
        Ok(node(&seg(&self.read(&path)?)?, k))
    }

    /// Every node of level `l` in the tree this snapshot reads, by index; `None` where not built.
    pub fn level(&self, l: u32) -> Result<Vec<Option<Node>>> {
        let mut out = Vec::new();
        self.walk(&level_dir(self.tree, l), |s, id| {
            let seg = seg(&self.fetch(id)?)?;
            for k in 0..seg.lines.len() {
                let i = (s * SEG) as usize + k;
                if out.len() <= i {
                    out.resize(i + 1, None);
                }
                out[i] = node(&seg, k);
            }
            Ok(())
        })?;
        Ok(out)
    }
}
