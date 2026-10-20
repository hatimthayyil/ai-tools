use std::{
    collections::{BTreeSet, HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    process,
    sync::{
        Arc,
        mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use tracing::{Span, dispatcher, info, info_span, warn};

use crate::{
    chat::{
        record::{Key, Line},
        rollup::{PROMPT, Rollup, Source},
        store::{Chat, Mark, Thread},
    },
    memory::{Backend, Block, ClaudeCode, JOBS, NODE, TRIES, Who, claude, log, write_line},
};

const MODEL: &str = "ai.chat.model";
const SUMMARISE: &str = "ai.chat.summarise";

/// The compactor's settings, from the repository's git config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// `ai.chat.model`; default the memory compactor's.
    pub model: String,
    /// `ai.chat.summarise`; default true: `sync-session` starts the background summariser.
    pub summarise: bool,
}

impl Chat {
    pub fn config(&self) -> Result<Config> {
        let c = self.repo().config_snapshot();
        let model = c
            .string(MODEL)
            .map(|m| m.to_string())
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(claude::model);
        let summarise = c
            .try_boolean(SUMMARISE)
            .with_context(|| format!("{SUMMARISE} is true or false"))?
            .unwrap_or(true);
        Ok(Config { model, summarise })
    }

    /// `model`, else the configured one, through the `claude` on `PATH`.
    pub fn backend(&self, model: Option<&str>) -> Result<Arc<dyn Backend>> {
        let model = match model {
            Some(m) => m.to_string(),
            None => self.config()?.model,
        };
        Ok(Arc::new(ClaudeCode::compactor_at(claude::find()?, &model)?))
    }

    /// Starts `ai chat summarise -t <name> --background`, unless `ai.chat.summarise` is false;
    /// whether it started.
    pub fn spawn_summariser(&self, thread: &Thread) -> Result<bool> {
        if !self.config()?.summarise {
            return Ok(false);
        }
        let repo = self.repo();
        let cwd = repo.workdir().unwrap_or(repo.git_dir());
        log::spawn(&thread.log_path()?, |c| {
            c.args(["chat", "summarise", "-t", thread.name(), "--background"])
                .current_dir(cwd)
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Built {
    /// Lines written, free ones included.
    pub lines: u64,
    /// Lines a model wrote.
    pub calls: u64,
    /// Lines whose call failed, and why.
    pub failed: Vec<(Key, String)>,
}

/// A dry run: what [`summarise`] or [`rebuild`] would write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dry {
    pub built: Built,
    /// Each line it would write, in build order: its first item, its key, its text and the
    /// text it would replace.
    pub lines: Vec<DryLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DryLine {
    pub first: u64,
    pub key: Key,
    pub text: String,
    pub was: Option<String>,
}

/// Under the thread's lock, waiting for it until `deadline`, builds `keys` and the unbuilt
/// lines they are made from, up to [`JOBS`] calls at once, until they are built, `deadline`
/// passes, or a line fails; logs to [`Thread::log_path`]. Calls still running at the deadline
/// are stopped through `backend`. A key that is no line of the thread is an error.
pub fn build(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    keys: &[Key],
    deadline: Instant,
) -> Result<Built> {
    let _log = log::init(&thread.log_path()?)?;
    let _build = info_span!("build", pid = process::id(), thread = thread.name()).entered();
    let lock = match thread.try_lock()? {
        Some(lock) => lock,
        None => {
            info!("busy: waiting for the compactor that holds the lock");
            let Some(lock) = thread.lock_before(deadline)? else {
                warn!("the lock was still held at the deadline; built nothing");
                return Ok(Built::default());
            };
            lock
        }
    };
    let mut run = Run::new(thread, backend, OnFail::Stop);
    run.pass(Some(keys), Some(deadline))?;
    let built = run.built.clone();
    drop(run);
    drop(lock);
    Ok(built)
}

/// Under the thread's lock, builds every line that is ready on any branch, over each newer
/// snapshot, until none is or failures stop it as `on_fail` says; logs to
/// [`Thread::log_path`]. `None` if another compactor holds the lock.
pub fn summarise(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    on_fail: OnFail,
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
    let mut run = Run::new(thread, backend, on_fail);
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
            let seen = run.pass(None, None)?;
            if run.tried() == tried {
                break seen;
            }
        };
        drop(lock);
        if run.halted() || thread.snapshot()?.mark() == seen {
            let b = &run.built;
            let secs = log::secs(start.elapsed());
            info!(
                lines = b.lines,
                calls = b.calls,
                failed = b.failed.len(),
                secs,
                "end"
            );
            return Ok(Some(run.built.clone()));
        }
    }
}

/// Rewrites every line of the thread: under its lock, waiting for it until `deadline`, drops
/// them all, then builds every line again as [`summarise`] does; logs to [`Thread::log_path`].
/// How many lines it dropped and what it built; `None` if another summariser still holds the
/// lock at the deadline.
pub fn rebuild(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    deadline: Instant,
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
    let mut run = Run::new(thread, backend, OnFail::Stop);
    loop {
        let tried = run.tried();
        run.pass(None, None)?;
        if run.tried() == tried {
            break;
        }
    }
    let built = run.built.clone();
    drop(run);
    drop(lock);
    let secs = log::secs(start.elapsed());
    info!(
        lines = built.lines,
        calls = built.calls,
        failed = built.failed.len(),
        secs,
        "end"
    );
    Ok(Some((dropped, built)))
}

/// What [`rewrite`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
/// at the deadline. A key that is no line of the thread is an error.
pub fn rewrite(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    key: Key,
    deadline: Instant,
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
    let mut run = Run::new(thread, backend, OnFail::Stop);
    loop {
        let tried = run.tried();
        run.pass(Some(&[key]), None)?;
        if run.tried() == tried {
            break;
        }
    }
    let built = run.built.clone();
    drop(run);
    drop(lock);
    info!(lines = built.lines, calls = built.calls, "end");
    Ok(Some(Rewrote {
        dropped,
        above: built_above.into_iter().filter(|&b| b).count(),
        built,
    }))
}

/// What [`summarise`] would write now, or with `rebuild` what [`rebuild`] would; with `line`,
/// what [`build`] of that line would, or with `rebuild` what [`rewrite`] would. Each line built
/// with the model and stored nowhere; logs to [`Thread::log_path`].
pub fn dry_run(
    thread: &Thread,
    backend: Arc<dyn Backend>,
    rebuild: bool,
    line: Option<Key>,
) -> Result<Dry> {
    let _log = log::init(&thread.log_path()?)?;
    let _span = info_span!(
        "dry_run",
        pid = process::id(),
        thread = thread.name(),
        model = backend.model()
    )
    .entered();
    info!(rebuild, "start");
    let mut run = Run::new(thread, backend, OnFail::Stop);
    run.dry = Some(Vec::new());
    run.anew = match (rebuild, line) {
        (false, _) => Anew::None,
        (true, None) => Anew::All,
        (true, Some(k)) => Anew::Line(k),
    };
    let want = line.map(|k| vec![k]);
    run.pass(want.as_deref(), None)?;
    let snap = thread.snapshot()?;
    let roll = Rollup::new(&snap.tree()?);
    let old = snap.all_lines()?;
    let mut built = run.dry.take().unwrap_or_default();
    built.sort_by_key(|(k, _)| order(*k));
    let lines = built
        .into_iter()
        .map(|(key, text)| DryLine {
            first: first(&roll, key),
            key,
            text,
            was: old.get(&key).map(|l| l.text.clone()),
        })
        .collect();
    Ok(Dry {
        built: run.built.clone(),
        lines,
    })
}

/// The first item line `key` covers.
pub fn first(roll: &Rollup, key: Key) -> u64 {
    match roll.source(key) {
        Some(Source::Chunk { items, .. } | Source::Turn(items)) => {
            items.first().copied().unwrap_or(key.last)
        }
        Some(Source::Chunked { message, chunks }) => {
            message.unwrap_or_else(|| chunks.first().map_or(key.last, |&c| first(roll, c)))
        }
        Some(Source::Node([a, _])) => first(roll, *a),
        None => key.last,
    }
}

type Done = (Key, Duration, Result<(Who, String)>);

/// Builds lines of one thread, each call on its own thread.
struct Run<'t> {
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
    /// A dry run's lines, kept here instead of stored; `None` stores them.
    dry: Option<Vec<(Key, String)>>,
    /// The stored lines read as unbuilt, to build them anew.
    anew: Anew,
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

impl<'t> Run<'t> {
    fn new(thread: &'t Thread, backend: Arc<dyn Backend>, on_fail: OnFail) -> Run<'t> {
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
            dry: None,
            anew: Anew::None,
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
    /// given, until nothing more can start or `deadline` passes; returns the state it read.
    fn pass(&mut self, want: Option<&[Key]>, deadline: Option<Instant>) -> Result<Mark> {
        let snap = self.thread.snapshot()?;
        let roll = Rollup::new(&snap.tree()?);
        let mut lines: HashMap<Key, String> = match self.anew {
            Anew::All => HashMap::new(),
            _ => snap
                .all_lines()?
                .into_iter()
                .map(|(k, l)| (k, l.text))
                .collect(),
        };
        if let Anew::Line(k) = self.anew {
            lines.remove(&k);
        }
        let mut todo: BTreeSet<(u64, u32)> = match want {
            Some(keys) => self.closure(&roll, &lines, keys)?,
            None => roll
                .lines()
                .keys()
                .filter(|k| !lines.contains_key(k))
                .map(|&k| order(k))
                .collect(),
        };
        loop {
            self.free(&roll, &mut lines, &mut todo)?;
            self.start(&roll, &lines, &todo);
            if self.busy.is_empty() {
                return Ok(snap.mark());
            }
            let done = match deadline {
                Some(d) => match self
                    .rx
                    .recv_timeout(d.saturating_duration_since(Instant::now()))
                {
                    Ok(done) => done,
                    Err(RecvTimeoutError::Timeout) => return Ok(snap.mark()),
                    Err(RecvTimeoutError::Disconnected) => unreachable!("the run holds a sender"),
                },
                None => self.rx.recv().expect("the run holds a sender"),
            };
            self.finish(&roll, done, &mut lines, &mut todo)?;
            while let Ok(done) = self.rx.try_recv() {
                self.finish(&roll, done, &mut lines, &mut todo)?;
            }
        }
    }

    /// The unbuilt lines among `keys` and those they are made from.
    fn closure(
        &self,
        roll: &Rollup,
        lines: &HashMap<Key, String>,
        keys: &[Key],
    ) -> Result<BTreeSet<(u64, u32)>> {
        let mut out = BTreeSet::new();
        let mut stack = keys.to_vec();
        while let Some(k) = stack.pop() {
            if lines.contains_key(&k) || !out.insert(order(k)) {
                continue;
            }
            let Some(s) = roll.source(k) else {
                bail!("{k} is not a line of thread {}.", self.thread.name());
            };
            stack.extend(s.children());
        }
        Ok(out)
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
            let texts: Vec<String> = match &mut self.dry {
                None => self
                    .thread
                    .put_lines(free)?
                    .into_iter()
                    .map(|l| l.text)
                    .collect(),
                Some(dry) => {
                    dry.extend(free.iter().map(|(k, l)| (*k, l.text.clone())));
                    free.into_iter().map(|(_, l)| l.text).collect()
                }
            };
            for (k, text) in keys.into_iter().zip(texts) {
                lines.insert(k, text);
                todo.remove(&order(k));
                self.built.lines += 1;
            }
        }
    }

    /// Starts the calls of `todo` that are ready, in order, up to [`JOBS`] running, unless
    /// failures halted the run. A line still over [`NODE`] bytes after [`TRIES`] fails.
    fn start(&mut self, roll: &Rollup, lines: &HashMap<Key, String>, todo: &BTreeSet<(u64, u32)>) {
        if self.halted() {
            return;
        }
        for k in todo.iter().map(|&o| key(o)) {
            if self.busy.len() >= JOBS {
                return;
            }
            if self.busy.contains(&k) || self.failed.contains(&k) {
                continue;
            }
            let Some(task) = roll.task(k, |c| lines.get(&c).map(String::as_str)) else {
                continue;
            };
            self.busy.insert(k);
            let (backend, tx) = (self.backend.clone(), self.tx.clone());
            let (span, dispatch) = (Span::current(), dispatcher::get_default(Clone::clone));
            thread::spawn(move || {
                let _d = dispatcher::set_default(&dispatch);
                let _s = span.enter();
                let at = Instant::now();
                let r = catch_unwind(AssertUnwindSafe(|| {
                    let (who, line) =
                        write_line(&*backend, PROMPT, &[Block::new(task, false)], &k)?;
                    if line.len() > NODE {
                        bail!(
                            "{} bytes after {TRIES} tries; a line holds at most {NODE}",
                            line.len()
                        );
                    }
                    Ok((who, line))
                }))
                .unwrap_or_else(|_| Err(anyhow!("the call panicked")));
                let _ = tx.send((k, at.elapsed(), r));
            });
        }
    }

    fn finish(
        &mut self,
        roll: &Rollup,
        (k, took, r): Done,
        lines: &mut HashMap<Key, String>,
        todo: &mut BTreeSet<(u64, u32)>,
    ) -> Result<()> {
        self.busy.remove(&k);
        let secs = log::secs(took);
        match r {
            Ok((who, text)) => {
                info!(line = %k, bytes = text.len(), secs, "built");
                let text = match &mut self.dry {
                    None => {
                        let kind = roll.source(k).expect("a planned line").kind();
                        self.thread.put_line(k, kind, &who, &text)?.text
                    }
                    Some(dry) => {
                        dry.push((k, text.clone()));
                        text
                    }
                };
                lines.insert(k, text);
                todo.remove(&order(k));
                self.built.lines += 1;
                self.built.calls += 1;
                self.streak = 0;
            }
            Err(e) => {
                let error = format!("{e:#}");
                warn!(line = %k, error, secs, "failed");
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

impl Drop for Run<'_> {
    fn drop(&mut self) {
        if !self.busy.is_empty() {
            self.backend.stop();
        }
    }
}
