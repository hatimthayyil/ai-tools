use std::{
    collections::{BTreeSet, HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    process,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use jiff::Timestamp;
use serde::Deserialize;
use tracing::{Span, dispatcher, info, info_span, warn};

use crate::{
    agent::{
        record::{Key, Line},
        rollup::{PROMPT, Rollup},
        store::{Mark, Snapshot, Store, Thread},
    },
    config::Settings,
    memory::{Backend, Block, JOBS, Spend, TRIES, Who, limit, log, write_line},
    progress::{Level, Progress},
};

/// `[chat]` in `ai.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Whether `sync-session` starts the background summariser; default true.
    pub background: Option<bool>,
}

impl Config {
    pub fn background(&self) -> bool {
        self.background.unwrap_or(true)
    }

    /// These settings, then `base`'s for those unset here.
    pub fn over(self, base: Config) -> Config {
        Config {
            background: self.background.or(base.background),
        }
    }
}

impl Store {
    /// What `ai.toml` sets where this store was found.
    pub fn settings(&self) -> Result<Settings> {
        Settings::find(self.cwd())
    }

    /// What `ai.toml` sets for chat where this store was found.
    pub fn config(&self) -> Result<Config> {
        Ok(self.settings()?.chat)
    }

    /// The `[summariser]` backend, with `model` over its model, keeping its calls in `thread`'s
    /// [`Thread::calls_dir`].
    pub fn backend(&self, thread: &Thread, model: Option<&str>) -> Result<Arc<dyn Backend>> {
        self.settings()?
            .summariser
            .backend(model, &thread.calls_dir())
    }

    /// The model a run uses: `model`, else the `[summariser]` one.
    pub fn model(&self, model: Option<&str>) -> Result<String> {
        Ok(match model {
            Some(m) => m.into(),
            None => self.settings()?.summariser.model(),
        })
    }

    /// Starts `ai agent build -a <name> --background`, unless `ai.toml` sets `chat.background`
    /// false; whether it started.
    pub fn spawn_summariser(&self, thread: &Thread) -> Result<bool> {
        if !self.config()?.background() {
            return Ok(false);
        }
        log::spawn(&thread.log_path()?, |c| {
            c.args(["agent", "build", "-a", thread.name(), "--background"])
                .current_dir(&self.project().dir)
        })?;
        Ok(true)
    }
}

/// What a run does once a line fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnFail {
    /// Starts no more calls: a request someone waits on.
    Stop,
    /// Builds what does not need the failed line, until [`JOBS`] calls fail in a row: the
    /// background summariser, which only logs.
    Continue,
}

/// What a compactor run did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Built {
    /// Lines written, free ones included.
    pub lines: u64,
    /// Lines a model wrote.
    pub calls: u64,
    /// Lines whose call failed, and why.
    pub failed: Vec<(Key, String)>,
    /// What the requests to the model took.
    pub spent: Spend,
}

/// A preview: what [`summarise`] or [`rebuild`] would write, written and stored nowhere.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preview {
    pub built: Built,
    /// Each line it would write, in build order: its first item, its key, its text and the
    /// text it would replace.
    pub lines: Vec<PreviewLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewLine {
    pub first: u64,
    pub key: Key,
    pub text: String,
    pub was: Option<String>,
}

/// Under the thread's lock, waiting for it however long, builds `keys` and the unbuilt lines
/// they are made from, up to [`JOBS`] calls at once, until they are built or a line fails;
/// logs to [`Thread::log_path`]. A key that is no line of the thread is an error. Shows its
/// progress on `progress`.
pub fn build(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    keys: &[Key],
    progress: &Progress,
) -> Result<Built> {
    let _log = log::init(&thread.log_path()?)?;
    let _build = info_span!("build", pid = process::id(), thread = thread.name()).entered();
    let lock = match thread.try_lock()? {
        Some(lock) => lock,
        None => {
            info!("busy: waiting for the compactor that holds the lock");
            thread.lock()?
        }
    };
    let mut run = Run::new(thread, backend, OnFail::Stop, progress);
    run.pass(Some(keys))?;
    let built = run.built.clone();
    drop(run);
    drop(lock);
    Ok(built)
}

/// Under the thread's lock, builds every line that is ready on any branch, over each newer
/// snapshot, until none is or failures stop it as `on_fail` says; logs to
/// [`Thread::log_path`]. `None` if another compactor holds the lock. Shows its progress on
/// `progress`.
pub fn summarise(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    on_fail: OnFail,
    progress: &Progress,
) -> Result<Option<Built>> {
    let _log = log::init(&thread.log_path()?)?;
    let _span = info_span!(
        "summarise",
        pid = process::id(),
        thread = thread.name(),
        model = backend.model()
    )
    .entered();
    let start = Instant::now();
    let mut run = Run::new(thread, backend, on_fail, progress);
    let mut started = false;
    loop {
        let Some(lock) = thread.try_lock()? else {
            info!("busy: another compactor holds the lock; exiting");
            return Ok(started.then(|| run.built.clone()));
        };
        if !started {
            info!("start");
            started = true;
        }
        let seen = loop {
            let tried = run.tried();
            let seen = run.pass(None)?;
            if run.tried() == tried {
                break seen;
            }
        };
        drop(lock);
        if run.halted() || thread.snapshot()?.mark() == seen {
            let b = &run.built;
            let secs = log::secs(start.elapsed());
            log::spend!(info, b.spent, {
                lines = b.lines,
                calls = b.calls,
                failed = b.failed.len(),
                secs
            }, "end");
            return Ok(Some(run.built.clone()));
        }
    }
}

/// Rewrites every line of the thread: under its lock, waiting for it until `deadline`, drops
/// them all, then builds every line again as [`summarise`] does; logs to [`Thread::log_path`].
/// How many lines it dropped and what it built; `None` if another summariser still holds the
/// lock at the deadline. Shows its progress on `progress`.
pub fn rebuild(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    deadline: Instant,
    progress: &Progress,
) -> Result<Option<(usize, Built)>> {
    let _log = log::init(&thread.log_path()?)?;
    let _span = info_span!(
        "rebuild",
        pid = process::id(),
        thread = thread.name(),
        model = backend.model()
    )
    .entered();
    let start = Instant::now();
    let Some(lock) = thread.lock_before(deadline)? else {
        warn!("the lock was still held at the deadline; rewrote nothing");
        return Ok(None);
    };
    info!("start");
    let dropped = thread.drop_lines()?;
    info!(dropped, "dropped");
    let mut run = Run::new(thread, backend, OnFail::Stop, progress);
    loop {
        let tried = run.tried();
        run.pass(None)?;
        if run.tried() == tried {
            break;
        }
    }
    let built = run.built.clone();
    drop(run);
    drop(lock);
    let secs = log::secs(start.elapsed());
    log::spend!(info, built.spent, {
        lines = built.lines,
        calls = built.calls,
        failed = built.failed.len(),
        secs
    }, "end");
    Ok(Some((dropped, built)))
}

/// What [`rewrite`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rewrote {
    /// Lines dropped: the line, if it was written, and those above it.
    pub dropped: usize,
    /// Of those, the lines above it, which the next [`summarise`] writes again.
    pub above: usize,
    pub built: Built,
}

/// Rewrites line `key`: under the thread's lock, waiting for it until `deadline`, drops it and
/// the lines made from it up to the top, in one commit, then writes it again from the lines
/// under it; logs to [`Thread::log_path`]. `None` if another summariser still holds the lock
/// at the deadline. A key that is no line of the thread is an error. Shows its progress on
/// `progress`.
pub fn rewrite(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    key: Key,
    deadline: Instant,
    progress: &Progress,
) -> Result<Option<Rewrote>> {
    let _log = log::init(&thread.log_path()?)?;
    let _span = info_span!(
        "rewrite",
        pid = process::id(),
        thread = thread.name(),
        line = %key,
        model = backend.model()
    )
    .entered();
    let Some(lock) = thread.lock_before(deadline)? else {
        warn!("the lock was still held at the deadline; rewrote nothing");
        return Ok(None);
    };
    let snap = thread.snapshot()?;
    let roll = Rollup::new(&snap.tree()?);
    if roll.source(key).is_none() {
        bail!("{key} is not a line of thread {}.", thread.name());
    }
    let above = roll.above(key);
    let built_above = above
        .iter()
        .map(|&k| snap.line(k).map(|l| l.is_some()))
        .collect::<Result<Vec<bool>>>()?;
    let dropped = thread.drop_keys(&[&[key][..], &above].concat())?;
    info!(dropped, "dropped");
    let mut run = Run::new(thread, backend, OnFail::Stop, progress);
    loop {
        let tried = run.tried();
        run.pass(Some(&[key]))?;
        if run.tried() == tried {
            break;
        }
    }
    let built = run.built.clone();
    drop(run);
    drop(lock);
    log::spend!(info, built.spent, { lines = built.lines, calls = built.calls }, "end");
    Ok(Some(Rewrote {
        dropped,
        above: built_above.into_iter().filter(|&b| b).count(),
        built,
    }))
}

/// What a run would write, per level of the thread's latest snapshot, as its plan shows it:
/// [`summarise`]'s, or with `rebuild` [`rebuild`]'s; with `line`, [`build`]'s of that line, or
/// with `rebuild` [`rewrite`]'s. Takes no lock, calls no model and stores nothing. A line that
/// is no line of the thread is an error.
pub fn plan(thread: &Thread, rebuild: bool, line: Option<Key>) -> Result<Vec<Level>> {
    let snap = thread.snapshot()?;
    let roll = Rollup::new(&snap.tree()?);
    let anew = match (rebuild, line) {
        (true, None) => Anew::All,
        _ => Anew::None,
    };
    let mut lines = written(&snap, anew)?;
    if let (true, Some(k)) = (rebuild, line) {
        if roll.source(k).is_none() {
            bail!("{k} is not a line of thread {}.", thread.name());
        }
        for d in [k].into_iter().chain(roll.above(k)) {
            lines.remove(&d);
        }
    }
    let want = line.map(|k| vec![k]);
    let todo = todo(thread.name(), &roll, &lines, want.as_deref())?;
    Ok(levels(&roll, &lines, &todo))
}

/// What [`summarise`] would write now, or with `rebuild` what [`rebuild`] would; with `line`,
/// what [`build`] of that line would, or with `rebuild` what [`rewrite`] would. Each line
/// written by the model and stored nowhere; logs to [`Thread::log_path`].
pub fn preview(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    rebuild: bool,
    line: Option<Key>,
) -> Result<Preview> {
    let _log = log::init(&thread.log_path()?)?;
    let _span = info_span!(
        "preview",
        pid = process::id(),
        thread = thread.name(),
        model = backend.model()
    )
    .entered();
    info!(rebuild, "start");
    let quiet = Progress::quiet();
    let mut run = Run::new(thread, backend, OnFail::Stop, &quiet);
    run.preview = Some(Vec::new());
    run.anew = match (rebuild, line) {
        (false, _) => Anew::None,
        (true, None) => Anew::All,
        (true, Some(k)) => Anew::Line(k),
    };
    let want = line.map(|k| vec![k]);
    run.pass(want.as_deref())?;
    let snap = thread.snapshot()?;
    let roll = Rollup::new(&snap.tree()?);
    let old = snap.all_lines()?;
    let mut built = run.preview.take().unwrap_or_default();
    built.sort_by_key(|(k, _)| order(*k));
    let lines = built
        .into_iter()
        .map(|(key, text)| PreviewLine {
            first: roll.first(key),
            key,
            text,
            was: old.get(&key).map(|l| l.text.clone()),
        })
        .collect();
    Ok(Preview {
        built: run.built.clone(),
        lines,
    })
}

type Done = (Key, Duration, Spend, Result<(Who, String)>);

/// Builds lines of one thread, each call on its own thread.
struct Run<'t, 'o> {
    thread: &'t Thread,
    backend: Arc<dyn Backend>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    busy: HashSet<Key>,
    failed: HashSet<Key>,
    on_fail: OnFail,
    /// Calls failed since the last that succeeded.
    streak: usize,
    built: Built,
    /// A preview's lines, kept here instead of stored; `None` stores them.
    preview: Option<Vec<(Key, String)>>,
    /// The stored lines read as unbuilt, to build them anew.
    anew: Anew,
    /// Shows the plan, the lines written as they are and the calls running.
    progress: &'t Progress<'o>,
}

/// Which stored lines a run reads as unbuilt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anew {
    None,
    All,
    Line(Key),
}

/// Build order: oldest first, and a line before the node over it that ends where it ends.
fn order(k: Key) -> (u64, u32) {
    (k.last, k.level)
}

fn key((last, level): (u64, u32)) -> Key {
    Key { level, last }
}

/// The lines of `snap` a run reads as written: every stored line, but those `anew` reads as
/// unbuilt.
fn written(snap: &Snapshot, anew: Anew) -> Result<HashMap<Key, String>> {
    let mut lines: HashMap<Key, String> = match anew {
        Anew::All => HashMap::new(),
        _ => snap
            .all_lines()?
            .into_iter()
            .map(|(k, l)| (k, l.text))
            .collect(),
    };
    if let Anew::Line(k) = anew {
        lines.remove(&k);
    }
    Ok(lines)
}

/// What a run writes of `roll`, `lines` written, in build order: `want` and the unbuilt lines
/// they are made from if given, else every unbuilt line. A key of `want` that is no line of
/// thread `name` is an error.
fn todo(
    name: &str,
    roll: &Rollup,
    lines: &HashMap<Key, String>,
    want: Option<&[Key]>,
) -> Result<BTreeSet<(u64, u32)>> {
    let Some(keys) = want else {
        return Ok(roll
            .lines()
            .keys()
            .filter(|k| !lines.contains_key(k))
            .map(|&k| order(k))
            .collect());
    };
    let mut out = BTreeSet::new();
    let mut stack = keys.to_vec();
    while let Some(k) = stack.pop() {
        if lines.contains_key(&k) || !out.insert(order(k)) {
            continue;
        }
        let Some(s) = roll.source(k) else {
            bail!("{k} is not a line of thread {name}.");
        };
        stack.extend(s.children());
    }
    Ok(out)
}

/// Per level, the lines of `roll`, those of `lines` written and those of `todo` left.
fn levels(roll: &Rollup, lines: &HashMap<Key, String>, todo: &BTreeSet<(u64, u32)>) -> Vec<Level> {
    let keys = roll.lines().keys();
    let top = keys
        .clone()
        .map(|k| k.level)
        .chain(todo.iter().map(|o| o.1))
        .max();
    let mut levels = vec![Level::default(); top.map_or(0, |t| t as usize + 1)];
    for k in keys {
        let v = &mut levels[k.level as usize];
        v.lines += 1;
        v.written += u64::from(lines.contains_key(k));
    }
    for &(_, l) in todo {
        levels[l as usize].left += 1;
    }
    levels
}

impl<'t, 'o> Run<'t, 'o> {
    fn new(
        thread: &'t Thread,
        backend: Arc<dyn Backend>,
        on_fail: OnFail,
        progress: &'t Progress<'o>,
    ) -> Run<'t, 'o> {
        let (tx, rx) = channel();
        Run {
            thread,
            backend,
            tx,
            rx,
            busy: HashSet::new(),
            failed: HashSet::new(),
            on_fail,
            streak: 0,
            built: Built::default(),
            preview: None,
            anew: Anew::None,
            progress,
        }
    }

    /// Lines written or failed so far.
    fn tried(&self) -> usize {
        self.built.lines as usize + self.built.failed.len()
    }

    /// Whether failures stop new calls: one under [`OnFail::Stop`], else [`JOBS`] in a row.
    fn halted(&self) -> bool {
        match self.on_fail {
            OnFail::Stop => !self.built.failed.is_empty(),
            OnFail::Continue => self.streak >= JOBS,
        }
    }

    /// Builds on the latest snapshot what is ready, of `want` and what it is made from if
    /// given, until nothing more can start; returns the state it read.
    fn pass(&mut self, want: Option<&[Key]>) -> Result<Mark> {
        let snap = self.thread.snapshot()?;
        let roll = Rollup::new(&snap.tree()?);
        let mut lines = written(&snap, self.anew)?;
        let mut todo = todo(self.thread.name(), &roll, &lines, want)?;
        self.show(&roll, &lines, &todo)?;
        loop {
            self.free(&roll, &mut lines, &mut todo)?;
            self.start(&roll, &lines, &todo);
            self.show(&roll, &lines, &todo)?;
            if self.busy.is_empty() {
                return Ok(snap.mark());
            }
            let done = self.rx.recv().expect("the run holds a sender");
            self.finish(&roll, done, &mut lines, &mut todo)?;
            while let Ok(done) = self.rx.try_recv() {
                self.finish(&roll, done, &mut lines, &mut todo)?;
            }
        }
    }

    /// Puts on the progress where the run stands, as [`levels`] counts it, and the levels of
    /// the calls running. Plans first if not yet.
    fn show(
        &self,
        roll: &Rollup,
        lines: &HashMap<Key, String>,
        todo: &BTreeSet<(u64, u32)>,
    ) -> Result<()> {
        let running: Vec<u32> = self.busy.iter().map(|k| k.level).collect();
        self.progress
            .update(&levels(roll, lines, todo), &running, &[])
    }

    /// Writes every line of `todo` that needs no call, in one commit per round, until none.
    fn free(
        &mut self,
        roll: &Rollup,
        lines: &mut HashMap<Key, String>,
        todo: &mut BTreeSet<(u64, u32)>,
    ) -> Result<()> {
        loop {
            let free: Vec<(Key, Line)> = todo
                .iter()
                .map(|&o| key(o))
                .filter_map(|k| {
                    let text = roll.free(k, |c| lines.get(&c).map(String::as_str))?;
                    let line = Line {
                        ts: Timestamp::now(),
                        kind: roll.source(k)?.kind(),
                        who: Who::default(),
                        text,
                    };
                    Some((k, line))
                })
                .collect();
            if free.is_empty() {
                return Ok(());
            }
            let keys: Vec<Key> = free.iter().map(|f| f.0).collect();
            let texts: Vec<String> = match &mut self.preview {
                None => self
                    .thread
                    .put_lines(free)?
                    .into_iter()
                    .map(|l| l.text)
                    .collect(),
                Some(preview) => {
                    preview.extend(free.iter().map(|(k, l)| (*k, l.text.clone())));
                    free.into_iter().map(|(_, l)| l.text).collect()
                }
            };
            for (k, text) in keys.into_iter().zip(texts) {
                lines.insert(k, text);
                todo.remove(&order(k));
                self.built.lines += 1;
                self.progress.joined(k.level, &roll.id(k))?;
            }
        }
    }

    /// Starts the calls of `todo` that are ready, in order, up to the backend's
    /// [`Backend::jobs`] running, unless failures halted the run. A line still over the
    /// [`limit`] of its words after [`TRIES`] fails.
    fn start(&mut self, roll: &Rollup, lines: &HashMap<Key, String>, todo: &BTreeSet<(u64, u32)>) {
        if self.halted() {
            return;
        }
        for k in todo.iter().map(|&o| key(o)) {
            if self.busy.len() >= self.backend.jobs() {
                return;
            }
            if self.busy.contains(&k) || self.failed.contains(&k) {
                continue;
            }
            let Some(task) = roll.task(k, |c| lines.get(&c).map(String::as_str)) else {
                continue;
            };
            let words = roll.words(k);
            self.busy.insert(k);
            let (backend, tx) = (self.backend.clone(), self.tx.clone());
            let (span, dispatch) = (Span::current(), dispatcher::get_default(Clone::clone));
            thread::spawn(move || {
                let _d = dispatcher::set_default(&dispatch);
                let _s = span.enter();
                let at = Instant::now();
                let mut spent = Spend::default();
                let r = catch_unwind(AssertUnwindSafe(|| {
                    let message = [Block::new(task)];
                    let (who, line) =
                        write_line(&*backend, PROMPT, &message, words, &k, &mut spent)?;
                    let (n, max) = (line.chars().count(), limit(words));
                    if n > max {
                        bail!("{n} characters after {TRIES} tries; this line holds at most {max}");
                    }
                    Ok((who, line))
                }))
                .unwrap_or_else(|_| Err(anyhow!("the call panicked")));
                let _ = tx.send((k, at.elapsed(), spent, r));
            });
        }
    }

    fn finish(
        &mut self,
        roll: &Rollup,
        (k, took, spent, r): Done,
        lines: &mut HashMap<Key, String>,
        todo: &mut BTreeSet<(u64, u32)>,
    ) -> Result<()> {
        self.busy.remove(&k);
        self.built.spent += spent;
        let secs = log::secs(took);
        match r {
            Ok((who, text)) => {
                let chars = text.chars().count();
                log::spend!(info, spent, { line = %k, chars, secs }, "built");
                let text = match &mut self.preview {
                    None => {
                        let kind = roll.source(k).expect("a planned line").kind();
                        self.thread.put_line(k, kind, &who, &text)?.text
                    }
                    Some(preview) => {
                        preview.push((k, text.clone()));
                        text
                    }
                };
                lines.insert(k, text);
                todo.remove(&order(k));
                self.built.lines += 1;
                self.built.calls += 1;
                self.streak = 0;
                self.progress
                    .built(k.level, &roll.id(k), took, chars, spent.all.cost)?;
            }
            Err(e) => {
                let error = format!("{e:#}");
                log::spend!(warn, spent, { line = %k, error, secs }, "failed");
                self.progress.failed(&roll.id(k), &error, spent.all.cost)?;
                self.failed.insert(k);
                self.built.failed.push((k, error));
                self.streak += 1;
                match self.on_fail {
                    OnFail::Stop if self.built.failed.len() == 1 => {
                        warn!("stopping: a line failed")
                    }
                    OnFail::Continue if self.streak == JOBS => {
                        warn!(failures = JOBS, "stopping: calls keep failing")
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

impl Drop for Run<'_, '_> {
    fn drop(&mut self) {
        if !self.busy.is_empty() {
            self.backend.stop();
        }
    }
}
