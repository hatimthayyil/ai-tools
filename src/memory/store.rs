use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, anyhow, bail};
use gix::ObjectId;

use crate::{
    file::{self, AtPath, pretty},
    git::{Changes, Ref, Snap, edit_config, fan_path},
    memory::{
        config::{Config, SECTION, SUBSECTION},
        meta::stamp,
        record::{COLUMNS, Message, Node, Who, now},
        tree::{Coord, free, joined},
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

pub fn level_dir(l: u32) -> String {
    format!("tree/{l}")
}

fn seg_of(c: Coord) -> (String, usize) {
    (fan_path(&level_dir(c.l), c.i / SEG), (c.i % SEG) as usize)
}

/// Nodes a mutation wrote, with their stored text.
pub type Built = Vec<(Coord, String)>;

pub struct Store {
    git: Ref,
    dir: PathBuf,
    pub cfg: Config,
    origin: String,
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
        let cfg = Config::load(&repo.config_snapshot(), &pretty(&dir.join("config")))?;
        let mut s = Store {
            git: Ref::new(repo, REF.into(), "memory", FORMAT),
            dir: dir.to_path_buf(),
            cfg,
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
                let s = Snapshot {
                    store: self,
                    git: self.git.tree(head, tree),
                };
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

    /// The origin in this store's config, created by the first open.
    fn load_origin(&self) -> Result<String> {
        let key = format!("{SECTION}.{SUBSECTION}.{ORIGIN}");
        if let Some(o) = self.git.repo().config_snapshot().string(key.as_str()) {
            return Ok(o.to_string());
        }
        self.edit_config(|f| {
            if let Some(o) = f.string_by(SECTION, Some(SUBSECTION.into()), ORIGIN) {
                return Ok(o.to_string());
            }
            let o = crockford(getrandom::u32()?, 6);
            f.set_raw_value_by(SECTION, Some(SUBSECTION.into()), ORIGIN, o.as_str())?;
            Ok(o)
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// This store's id, stamped on every message and node it writes.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Replaces this store's id in its config.
    pub fn set_origin(&mut self, origin: &str) -> Result<()> {
        if origin.is_empty() || origin.contains(char::is_whitespace) {
            bail!("'{origin}' is not an origin: one word.");
        }
        self.edit_config(|f| {
            f.set_raw_value_by(SECTION, Some(SUBSECTION.into()), ORIGIN, origin)?;
            Ok(())
        })?;
        self.origin = origin.into();
        Ok(())
    }

    pub fn head(&self) -> Result<Option<ObjectId>> {
        self.git.head()
    }

    /// The memory as of the latest commit.
    pub fn snapshot(&self) -> Result<Snapshot<'_>> {
        self.at(self.head()?)
    }

    /// The memory as of `commit`, which must be of [`FORMAT`]; `None` is the empty memory.
    fn at(&self, commit: Option<ObjectId>) -> Result<Snapshot<'_>> {
        let git = self.git.raw(commit)?;
        let v = git.format()?;
        self.git.readable(v)?;
        if v < FORMAT {
            bail!(
                "This memory is format {v}, older than this ai's {FORMAT}. Run {ME} wake to upgrade it."
            );
        }
        Ok(Snapshot { store: self, git })
    }

    /// Applies `change` to the latest snapshot as one commit, redoing it until the ref update wins.
    pub fn mutate<R>(
        &self,
        msg: &str,
        change: impl FnMut(&Snapshot) -> Result<(R, Changes)>,
    ) -> Result<(R, Snapshot<'_>)> {
        self.git.mutate(msg, |c| self.at(c), change)
    }

    fn edit_config<R>(&self, edit: impl FnOnce(&mut gix::config::File) -> Result<R>) -> Result<R> {
        edit_config(&self.dir.join("config"), edit)
    }

    pub fn save_config(&self, cfg: &Config) -> Result<()> {
        self.edit_config(|f| cfg.save(f))
    }

    /// Appends `items` in order, with every free node they complete; returns the first new id.
    /// A message without an origin gets this store's, one without a ts the time it lands, never
    /// before the last message's: the log is in time order. Every other field is kept as given.
    pub fn append(&self, msg: &str, items: &[Message]) -> Result<(u64, Built, Snapshot<'_>)> {
        let ((first, built), snap) = self.mutate(msg, |s| {
            let mut ed = Edit::new(s)?;
            let first = ed.t;
            for m in items {
                ed.push(m)?;
            }
            let (built, ch) = ed.finish();
            Ok(((first, built), ch))
        })?;
        Ok((first, built, snap))
    }

    /// Writes node `c` unless it is built, with every free node above it it completes.
    pub fn put_node(&self, c: Coord, who: &Who, text: &str) -> Result<(Built, Snapshot<'_>)> {
        self.mutate(&format!("node {c}"), |s| {
            let mut ed = Edit::new(s)?;
            if c.end() > ed.t {
                bail!("Node {c} is beyond the log: it holds {} messages.", ed.t);
            }
            if ed.text(c)?.is_none() {
                ed.put(c, who, text)?;
            }
            Ok(ed.finish())
        })
    }
}

/// One mutation: new messages and nodes over a snapshot, free nodes completed as they become ready.
struct Edit<'a, 's> {
    snap: &'a Snapshot<'s>,
    t: u64,
    ch: Changes,
    segs: BTreeMap<String, Seg>,
    built: Built,
    last: Option<String>,
}

impl<'a, 's> Edit<'a, 's> {
    fn new(snap: &'a Snapshot<'s>) -> Result<Self> {
        Ok(Edit {
            snap,
            t: snap.log_len()?,
            ch: Changes::default(),
            segs: BTreeMap::new(),
            built: Vec::new(),
            last: None,
        })
    }

    fn seg(&mut self, path: &str) -> Result<&mut Seg> {
        if !self.segs.contains_key(path) {
            let seg = seg(&self.snap.read(path)?)?;
            self.segs.insert(path.into(), seg);
        }
        Ok(self.segs.get_mut(path).expect("inserted above"))
    }

    fn text(&mut self, c: Coord) -> Result<Option<String>> {
        let (path, k) = seg_of(c);
        Ok(node(self.seg(&path)?, k).map(|n| n.text))
    }

    fn put(&mut self, c: Coord, who: &Who, text: &str) -> Result<()> {
        let node = Node {
            ts: now(),
            origin: self.snap.store.origin.clone(),
            who: who.clone(),
            text: text.into(),
        };
        let (path, k) = seg_of(c);
        let seg = self.seg(&path)?;
        let line = node.encode(&seg.cols);
        let text = Node::decode(&line, &seg.cols)
            .context("An empty node.")?
            .text;
        if seg.lines.len() <= k {
            seg.lines.resize(k + 1, String::new());
        }
        seg.lines[k] = line;
        self.built.push((c, text));
        self.cascade(c)
    }

    fn cascade(&mut self, c: Coord) -> Result<()> {
        let p = c.parent();
        if p.end() > self.t || self.text(p)?.is_some() {
            return Ok(());
        }
        let [a, b] = p.children();
        let (Some(a), Some(b)) = (self.text(a)?, self.text(b)?) else {
            return Ok(());
        };
        if a.is_empty() || b.is_empty() {
            return self.put(p, &Who::default(), &joined(&a, &b));
        }
        match free(joined(&a, &b)) {
            Some(text) => self.put(p, &Who::default(), &text),
            None => Ok(()),
        }
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
        match free(m.line()) {
            Some(text) => self.put(Coord::leaf(i), &Who::default(), &text),
            None => Ok(()),
        }
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

/// The memory as of one commit: every read through it sees the same state.
pub struct Snapshot<'s> {
    store: &'s Store,
    git: Snap<'s>,
}

impl<'s> AsRef<Snap<'s>> for Snapshot<'s> {
    fn as_ref(&self) -> &Snap<'s> {
        &self.git
    }
}

impl Snapshot<'_> {
    pub fn cfg(&self) -> &Config {
        &self.store.cfg
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

    /// Node `c`, if built.
    pub fn node(&self, c: Coord) -> Result<Option<Node>> {
        let (path, k) = seg_of(c);
        Ok(node(&seg(&self.read(&path)?)?, k))
    }

    /// Every node of level `l`, by index; `None` where not built.
    pub fn level(&self, l: u32) -> Result<Vec<Option<Node>>> {
        let mut out = Vec::new();
        self.walk(&level_dir(l), |s, id| {
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
