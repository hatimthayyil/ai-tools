//! What a summariser run shows while it builds a store's lines: its plan first, then one bar per
//! level of the store's tree and a total, with a line for each line written as it happens, then
//! what it did. On a terminal the bars draw on stderr with those lines above them; off one, the
//! lines go plain to the run's output; a quiet run shows nothing.

use std::{
    cell::RefCell,
    fmt::{self, Display},
    fs::File,
    io::{self, IsTerminal, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Result;
use indicatif::{MultiProgress, ProgressBar, ProgressState, ProgressStyle};

use crate::{
    file::{AtPath, pretty},
    memory::{Spend, flat, plural},
};

/// How many of a log's newest `built` events [`Past`] averages.
const SAMPLES: usize = 200;
/// How much of a log's end [`Past`] reads.
const TAIL: u64 = 1 << 20;
/// The most characters of an error an event line shows; the log keeps it whole.
const ERROR: usize = 160;

/// Whether stderr is a terminal, where bars can draw.
pub fn terminal() -> bool {
    io::stderr().is_terminal()
}

/// How a run shows itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Nothing: the background summariser, and runs inside other commands.
    Quiet,
    /// The plan, a line per event and the end, to the run's output.
    Plain,
    /// The plan and the end to the run's output; bars with the events above them on stderr.
    Bars,
}

impl Mode {
    /// [`Mode::Bars`] when `show` and stderr is a [`terminal`], [`Mode::Plain`] when only
    /// `show`, else [`Mode::Quiet`].
    pub fn of(show: bool) -> Mode {
        match (show, terminal()) {
            (false, _) => Mode::Quiet,
            (true, true) => Mode::Bars,
            (true, false) => Mode::Plain,
        }
    }
}

/// One level of a store's tree as a run sees it: the lines the level holds, those of them
/// written, and those left for the run to write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Level {
    pub lines: u64,
    pub written: u64,
    pub left: u64,
}

/// What a run builds, for its plan and its end.
pub struct About {
    /// What the run does, opening its plan: `Rebuilding memory`.
    pub title: String,
    /// What it writes: `summary`, `line`.
    pub unit: &'static str,
    /// What one line of level `l` covers: `8 messages`, `4 turns`.
    pub covers: fn(u32) -> String,
    /// The lowest level with a bar: the levels under it hold lines no run writes.
    pub first_bar: u32,
    pub model: String,
    /// The calls it runs at once.
    pub jobs: usize,
    /// Its log, the full record of the run.
    pub log: PathBuf,
    pub past: Past,
}

impl Default for About {
    fn default() -> About {
        About {
            title: String::new(),
            unit: "line",
            covers: |l| format!("level {l}"),
            first_bar: 0,
            model: String::new(),
            jobs: 1,
            log: PathBuf::new(),
            past: Past::default(),
        }
    }
}

/// The calls a store's summariser made before: the level, seconds and cost of each line a
/// model wrote, from the `built` events of its log, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Past(Vec<(u32, f64, f64)>);

impl Past {
    /// The last [`SAMPLES`] `built` events of the log at `path`, each line's level read from
    /// its id by `level`; none without a log.
    pub fn read(path: &Path, level: impl Fn(&str) -> Option<u32>) -> Result<Past> {
        let mut f = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Past::default()),
            Err(e) => return Err(e).at(path),
        };
        let from = f.metadata().at(path)?.len().saturating_sub(TAIL);
        f.seek(SeekFrom::Start(from)).at(path)?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).at(path)?;
        let text = String::from_utf8_lossy(&bytes);
        let lines = text.lines().skip(usize::from(from > 0));
        let mut calls: Vec<_> = lines.filter_map(|l| built(l, &level)).collect();
        let old = calls.len().saturating_sub(SAMPLES);
        Ok(Past(calls.split_off(old)))
    }

    /// How many calls it holds.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The seconds and cost of a call at level `l`: the average of that level's calls, else of
    /// all; `None` without any.
    fn call(&self, l: u32) -> Option<(f64, f64)> {
        mean(self.0.iter().filter(|c| c.0 == l)).or_else(|| self.mean())
    }

    /// The average seconds and cost of a call; `None` without any.
    fn mean(&self) -> Option<(f64, f64)> {
        mean(self.0.iter())
    }
}

/// The average seconds and cost of `calls`; `None` without any.
fn mean<'a>(calls: impl Iterator<Item = &'a (u32, f64, f64)>) -> Option<(f64, f64)> {
    let (n, secs, cost) = calls.fold((0.0, 0.0, 0.0), |(n, s, c), x| (n + 1.0, s + x.1, c + x.2));
    (n > 0.0).then(|| (secs / n, cost / n))
}

/// The level, seconds and cost of log line `line` when it is a `built` event, its id given a
/// level by `level`.
fn built(line: &str, level: impl Fn(&str) -> Option<u32>) -> Option<(u32, f64, f64)> {
    let (_, fields) = line.split_once(": built ")?;
    let field = |k: &str| {
        fields
            .split(' ')
            .find_map(|f| f.strip_prefix(k)?.strip_prefix('='))
    };
    let id = field("node").or_else(|| field("line"))?;
    Some((
        level(id)?,
        field("secs")?.parse().ok()?,
        field("cost")?.parse().ok()?,
    ))
}

/// What a run did at one level: lines written, and where its other lines stand.
#[derive(Clone, Copy, Debug, Default)]
struct Tally {
    done: u64,
    left: u64,
    running: u64,
    waiting: u64,
}

/// A run's display. Its events and updates come from the thread that runs the calls.
pub struct Progress<'o> {
    about: About,
    mode: Mode,
    inner: RefCell<Inner<'o>>,
}

struct Inner<'o> {
    out: Option<&'o mut dyn Write>,
    /// Whether the plan is out, and with it the bars.
    planned: bool,
    bars: Option<Bars>,
    start: Instant,
    levels: Vec<Tally>,
    /// Lines the model wrote, and the seconds their calls took.
    built: u64,
    secs: f64,
    joined: u64,
    /// Calls tried again after failing.
    retries: u64,
    cost: f64,
}

impl<'o> Progress<'o> {
    /// A run about `about` showing itself as `mode` says, to `out` and stderr.
    pub fn new(out: &'o mut dyn Write, mode: Mode, about: About) -> Progress<'o> {
        Progress::with(Some(out), mode, about)
    }

    /// A run that shows nothing.
    pub fn quiet() -> Progress<'static> {
        Progress::with(None, Mode::Quiet, About::default())
    }

    fn with(out: Option<&'o mut dyn Write>, mode: Mode, about: About) -> Progress<'o> {
        Progress {
            about,
            mode,
            inner: RefCell::new(Inner {
                out,
                planned: false,
                bars: None,
                start: Instant::now(),
                levels: Vec::new(),
                built: 0,
                secs: 0.0,
                joined: 0,
                retries: 0,
                cost: 0.0,
            }),
        }
    }

    /// Prints the plan of a run over `levels`, unless it has nothing to write, and on a
    /// terminal starts its bars; once. Whether the plan has anything to write.
    pub fn plan(&self, levels: &[Level]) -> Result<bool> {
        let mut i = self.inner.borrow_mut();
        if i.planned {
            return Ok(true);
        }
        if levels.iter().all(|l| l.left == 0) {
            return Ok(false);
        }
        i.planned = true;
        i.start = Instant::now();
        if self.mode == Mode::Quiet {
            return Ok(true);
        }
        let text = plan_text(&self.about, levels);
        if let Some(out) = i.out.as_mut() {
            out.write_all(text.as_bytes())?;
            out.flush()?;
        }
        if self.mode == Mode::Bars {
            i.bars = Some(Bars::new(&self.about, levels.len()));
        }
        Ok(true)
    }

    /// Whether a plan with something to write is out.
    pub fn planned(&self) -> bool {
        self.inner.borrow().planned
    }

    /// Where the run stands: per level, its lines, those written and those left; the levels of
    /// the calls running, and of those waiting to be tried again. Plans first if not yet.
    pub fn update(&self, levels: &[Level], running: &[u32], waiting: &[u32]) -> Result<()> {
        if !self.plan(levels)? {
            return Ok(());
        }
        let mut i = self.inner.borrow_mut();
        let n = levels.len().max(i.levels.len());
        i.levels.resize(n, Tally::default());
        for (l, t) in i.levels.iter_mut().enumerate() {
            t.left = levels.get(l).map_or(0, |v| v.left);
            t.running = running.iter().filter(|&&r| r as usize == l).count() as u64;
            t.waiting = waiting.iter().filter(|&&w| w as usize == l).count() as u64;
        }
        i.draw(&self.about);
        Ok(())
    }

    /// The model wrote line `id` of level `l`: `chars` characters, in `took`, at `cost`.
    pub fn built(
        &self,
        l: u32,
        id: &dyn Display,
        took: Duration,
        chars: usize,
        cost: f64,
    ) -> Result<()> {
        let mut i = self.inner.borrow_mut();
        i.wrote(l);
        i.built += 1;
        i.secs += took.as_secs_f64();
        i.cost += cost;
        let what = format!("{:.1}s  {chars} chars  {}", took.as_secs_f64(), usd(cost));
        self.event(&mut i, "built", id, &what)
    }

    /// Line `id` of level `l` was written from what it is made from, which fits one line,
    /// without a call.
    pub fn joined(&self, l: u32, id: &dyn Display) -> Result<()> {
        let mut i = self.inner.borrow_mut();
        i.wrote(l);
        i.joined += 1;
        self.event(&mut i, "joined", id, "")
    }

    /// The call for line `id` failed with `error`, at `cost`; try `next` of `of` starts after
    /// `wait`.
    pub fn retry(
        &self,
        id: &dyn Display,
        error: &str,
        (next, of): (usize, usize),
        wait: Duration,
        cost: f64,
    ) -> Result<()> {
        let mut i = self.inner.borrow_mut();
        i.retries += 1;
        i.cost += cost;
        let what = format!("try {next} of {of} in {}: {}", clock(wait), short(error));
        self.event(&mut i, "retry", id, &what)
    }

    /// The call for line `id` failed with `error`, at `cost`, and is not tried again.
    pub fn failed(&self, id: &dyn Display, error: &str, cost: f64) -> Result<()> {
        let mut i = self.inner.borrow_mut();
        i.cost += cost;
        self.event(&mut i, "failed", id, &short(error))
    }

    /// The run gave up on line `id` after `tries` failures, the last with `error`.
    pub fn gave_up(&self, id: &dyn Display, tries: usize, error: &str) -> Result<()> {
        let mut i = self.inner.borrow_mut();
        let what = format!("after {tries} tries: {}", short(error));
        self.event(&mut i, "gave up", id, &what)
    }

    /// Shows one event: above the bars, or plain after the run's count so far.
    fn event(&self, i: &mut Inner, verb: &str, id: &dyn Display, what: &str) -> Result<()> {
        let line = format!("{verb:>7} {:<10} {what}", id.to_string());
        let line = line.trim_end();
        match self.mode {
            Mode::Quiet => {}
            Mode::Bars => {
                if let Some(b) = &i.bars {
                    b.multi.println(line)?;
                }
                i.draw(&self.about);
            }
            Mode::Plain => {
                let (done, all) = i.count();
                let w = all.to_string().len();
                if let Some(out) = i.out.as_mut() {
                    writeln!(out, "[{done:>w$}/{all}] {line}")?;
                    out.flush()?;
                }
            }
        }
        Ok(())
    }

    /// Clears the bars and prints what the run did, after it `dropped` lines first if it
    /// rebuilt, with what its requests took, `spent`.
    pub fn end(self, dropped: Option<u64>, spent: Spend) -> Result<()> {
        let About { unit, log, .. } = &self.about;
        let mut i = self.inner.into_inner();
        drop(i.bars.take());
        if self.mode == Mode::Quiet {
            return Ok(());
        }
        let Some(out) = i.out.as_mut() else {
            return Ok(());
        };
        write!(
            out,
            "Built {} in {}: {} by the model, {} joined.",
            plural(i.built + i.joined, unit),
            clock(i.start.elapsed()),
            i.built,
            i.joined
        )?;
        if let Some(d) = dropped {
            write!(out, " Dropped {} first.", plural(d, unit))?;
        }
        writeln!(
            out,
            "\n{} ({} to shorten a line, {} after a failure), {}, {} tokens.\nLog: {}",
            plural(spent.requests, "call"),
            spent.retries,
            i.retries,
            usd(spent.all.cost),
            tokens(spent.all.tokens()),
            pretty(log)
        )?;
        Ok(())
    }
}

impl Inner<'_> {
    /// Counts a line of level `l` written: one more done, one fewer left until the next update.
    fn wrote(&mut self, l: u32) {
        let l = l as usize;
        if self.levels.len() <= l {
            self.levels.resize(l + 1, Tally::default());
        }
        let t = &mut self.levels[l];
        t.done += 1;
        t.left = t.left.saturating_sub(1);
    }

    /// Lines written so far, and those planned: written and left.
    fn count(&self) -> (u64, u64) {
        let done: u64 = self.levels.iter().map(|t| t.done).sum();
        let left: u64 = self.levels.iter().map(|t| t.left).sum();
        (done, done + left)
    }

    /// Puts the tallies on the bars, adding one for a level that has none yet.
    fn draw(&mut self, about: &About) {
        let Some(bars) = self.bars.as_mut() else {
            return;
        };
        while bars.next() < self.levels.len() as u32 {
            bars.add(about);
        }
        let shown = self.levels.iter().skip(bars.first as usize);
        for (b, t) in bars.levels.iter().zip(shown) {
            b.set_length(t.done + t.left);
            b.set_position(t.done);
            b.set_message(state(t));
        }
        let (done, all) = (
            self.levels.iter().map(|t| t.done).sum::<u64>(),
            self.levels.iter().map(|t| t.done + t.left).sum::<u64>(),
        );
        let (running, waiting): (u64, u64) = (
            self.levels.iter().map(|t| t.running).sum(),
            self.levels.iter().map(|t| t.waiting).sum(),
        );
        bars.total.set_length(all);
        bars.total.set_position(done);
        let mut msg = format!("{running} running");
        if waiting > 0 {
            msg += &format!(" · {waiting} retrying");
        }
        msg += &format!(" · {}", usd(self.cost));
        bars.total.set_message(msg);
        let per = match self.built {
            0 => about.past.mean().map(|c| c.0),
            n => Some(self.secs / n as f64),
        };
        let left = per.map(|s| (all - done) as f64 * s / about.jobs.max(1) as f64);
        *bars.left.lock().expect("a held lock") = match left {
            Some(s) if all > done => format!(" · ~{} left", clock(Duration::from_secs_f64(s))),
            _ => String::new(),
        };
    }
}

/// What a level's bar says: done, nothing to write, or its calls.
fn state(t: &Tally) -> String {
    match (t.done + t.left, t.left) {
        (0, _) => "nothing to write".into(),
        (_, 0) => "done".into(),
        _ => match (t.running, t.waiting) {
            (0, 0) => "waiting".into(),
            (r, 0) => format!("{r} running"),
            (0, w) => format!("{w} retrying"),
            (r, w) => format!("{r} running · {w} retrying"),
        },
    }
}

/// A run's bars on stderr: one per level from `first`, lowest first, then the total.
struct Bars {
    multi: MultiProgress,
    first: u32,
    levels: Vec<ProgressBar>,
    total: ProgressBar,
    /// The total's estimate of the time left, after its clock.
    left: Arc<Mutex<String>>,
}

impl Bars {
    fn new(about: &About, levels: usize) -> Bars {
        let multi = MultiProgress::new();
        let left = Arc::new(Mutex::new(String::new()));
        let shown = left.clone();
        let style = ProgressStyle::with_template(
            "{prefix} {bar:30} {pos:>5}/{len:<5} {msg} · {time}{left}",
        )
        .expect("a valid template")
        .progress_chars("█░")
        .with_key("time", |s: &ProgressState, w: &mut dyn fmt::Write| {
            let _ = w.write_str(&clock(s.elapsed()));
        })
        .with_key("left", move |_: &ProgressState, w: &mut dyn fmt::Write| {
            let _ = w.write_str(&shown.lock().expect("a held lock"));
        });
        let total = multi.add(ProgressBar::new(0).with_style(style).with_prefix(format!(
            "{:<w$}",
            "Total",
            w = prefix_width(about)
        )));
        total.enable_steady_tick(Duration::from_millis(250));
        let mut bars = Bars {
            multi,
            first: about.first_bar,
            levels: Vec::new(),
            total,
            left,
        };
        while bars.next() < levels as u32 {
            bars.add(about);
        }
        bars
    }

    /// The level the next bar is for.
    fn next(&self) -> u32 {
        self.first + self.levels.len() as u32
    }

    /// The bar of the [`next`](Bars::next) level, above the total.
    fn add(&mut self, about: &About) {
        let style = ProgressStyle::with_template("{prefix} {bar:30} {pos:>5}/{len:<5} {msg}")
            .expect("a valid template")
            .progress_chars("█░");
        let bar = ProgressBar::new(0)
            .with_style(style)
            .with_prefix(level_name(about, self.next()));
        self.levels.push(self.multi.insert_before(&self.total, bar));
    }
}

impl Drop for Bars {
    fn drop(&mut self) {
        for b in self.levels.iter().chain([&self.total]) {
            b.finish_and_clear();
        }
        let _ = self.multi.clear();
    }
}

/// `L3  8 messages`, padded to [`prefix_width`].
fn level_name(about: &About, l: u32) -> String {
    let name = format!("L{l:<3}{}", (about.covers)(l));
    format!("{name:<w$}", w = prefix_width(about))
}

/// The width of the bars' names: wide enough for the levels of a tree of a billion lines.
fn prefix_width(about: &About) -> usize {
    4 + (0..30).map(|l| (about.covers)(l).len()).max().unwrap_or(0)
}

/// The plan of a run over `levels`: what it is, a row per level, its calls, their estimated
/// cost and time, and its log.
fn plan_text(about: &About, levels: &[Level]) -> String {
    let mut s = format!(
        "{} with {}, {} at once.\n",
        about.title,
        about.model,
        plural(about.jobs as u64, "call")
    );
    let names: Vec<String> = (0..levels.len() as u32)
        .map(|l| format!("L{l:<3}{}", (about.covers)(l)))
        .collect();
    let w = names.iter().map(String::len).max().unwrap_or(0).max(12);
    s += &format!(
        "  {:<w$}  {:>7}  {:>7}  {:>8}\n",
        "level", "lines", "written", "to write"
    );
    for (name, l) in names.iter().zip(levels) {
        s += &format!(
            "  {name:<w$}  {:>7}  {:>7}  {:>8}\n",
            l.lines, l.written, l.left
        );
    }
    let calls: u64 = levels.iter().map(|l| l.left).sum();
    s += &format!(
        "Up to {}: a line whose sources fit in one is joined without a call.\n",
        plural(calls, "model call")
    );
    let (mut secs, mut cost) = (0.0, 0.0);
    for (l, v) in levels.iter().enumerate() {
        if let Some((t, c)) = about.past.call(l as u32) {
            secs += v.left as f64 * t;
            cost += v.left as f64 * c;
        }
    }
    s += &match about.past.len() {
        0 => "No past calls to estimate cost and time from.\n".into(),
        n => format!(
            "About {} and {}, at the average of the last {}.\n",
            usd(cost),
            clock(Duration::from_secs_f64(secs / about.jobs.max(1) as f64)),
            plural(n as u64, "call")
        ),
    };
    s + &format!("Log: {}\n", pretty(&about.log))
}

/// `error` on one line, cut to [`ERROR`] characters.
fn short(error: &str) -> String {
    let e = flat(error);
    match e.char_indices().nth(ERROR) {
        Some((at, _)) => format!("{}…", &e[..at]),
        None => e,
    }
}

/// `$0.0051` under a dollar, else `$2.91`.
fn usd(cost: f64) -> String {
    match cost < 1.0 {
        true => format!("${cost:.4}"),
        false => format!("${cost:.2}"),
    }
}

/// `512`, `45.6k`, `1.23M`.
fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.2}M", n as f64 / 1e6),
    }
}

/// `42s`, `14m 03s`, `2h 05m`.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn about(past: Past) -> About {
        About {
            title: "Rebuilding memory".into(),
            unit: "summary",
            covers: |l| plural(1 << l, "message"),
            first_bar: 1,
            model: "sonnet".into(),
            jobs: 8,
            log: PathBuf::from("/m/summarise.log"),
            past,
        }
    }

    #[test]
    fn bars_start_at_the_first_level_a_run_writes() {
        let mut out = Vec::new();
        let p = Progress::new(&mut out, Mode::Bars, about(Past::default()));
        let shown = |p: &Progress| {
            let i = p.inner.borrow();
            let bars = i.bars.as_ref().expect("bars once planned");
            let shown = bars.levels.iter().map(|b| (b.prefix(), b.length()));
            shown.collect::<Vec<_>>()
        };
        let name = |l| level_name(&about(Past::default()), l);
        p.update(&[level(4, 4), level(2, 0), level(1, 0)], &[], &[])
            .unwrap();
        assert_eq!(shown(&p), [(name(1), Some(2)), (name(2), Some(1))]);
        p.update(
            &[level(5, 5), level(2, 0), level(1, 0), level(1, 0)],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(shown(&p)[2], (name(3), Some(1)));
    }

    fn level(lines: u64, written: u64) -> Level {
        Level {
            lines,
            written,
            left: lines - written,
        }
    }

    #[test]
    fn a_plan_has_a_row_per_level_its_calls_and_an_estimate() {
        let past = Past(vec![(1, 4.0, 0.004), (1, 6.0, 0.006), (2, 10.0, 0.01)]);
        let text = plan_text(
            &about(past),
            &[level(9, 9), level(4, 1), level(2, 0), level(1, 0)],
        );
        assert_eq!(
            text,
            "Rebuilding memory with sonnet, 8 calls at once.\n\
             \x20 level             lines  written  to write\n\
             \x20 L0  1 message         9        9         0\n\
             \x20 L1  2 messages        4        1         3\n\
             \x20 L2  4 messages        2        0         2\n\
             \x20 L3  8 messages        1        0         1\n\
             Up to 6 model calls: a line whose sources fit in one is joined without a call.\n\
             About $0.0417 and 5s, at the average of the last 3 calls.\n\
             Log: /m/summarise.log\n"
        );
    }

    #[test]
    fn a_plan_without_past_calls_says_so() {
        let text = plan_text(&about(Past::default()), &[level(1, 0)]);
        assert!(
            text.contains("\nNo past calls to estimate cost and time from.\n"),
            "{text}"
        );
    }

    #[test]
    fn past_calls_are_read_from_built_events() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("summarise.log");
        std::fs::write(
            &log,
            "x INFO summarise{pid=1}: ai_tools::memory::compact: built node=920+4 chars=413 secs=3.5 requests=1 cost=0.0057 retry_cost=0.0\n\
             x INFO summarise{pid=1}: ai_tools::memory::cli: end built=1 secs=3.4 cost=0.0057\n\
             x INFO build{pid=2}: ai_tools::agent::summarise: built line=1@2834 chars=417 secs=5.5 cost=0.0047\n",
        )
        .unwrap();
        let past = Past::read(&log, |id| match id.split_once('@') {
            Some((l, _)) => l.parse().ok(),
            None => Some(2),
        })
        .unwrap();
        assert_eq!(past, Past(vec![(2, 3.5, 0.0057), (1, 5.5, 0.0047)]));
        assert_eq!(past.call(1), Some((5.5, 0.0047)));
        assert_eq!(past.call(7), Some((4.5, 0.0052)));
        assert!(
            Past::read(&dir.path().join("none"), |_| Some(0))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn plain_runs_print_the_plan_each_event_and_the_end() {
        let mut out = Vec::new();
        let p = Progress::new(&mut out, Mode::Plain, about(Past::default()));
        let levels = [level(2, 2), level(1, 0)];
        p.update(&levels, &[1], &[]).unwrap();
        p.retry(
            &"0+2",
            "429\nrate limited",
            (2, 9),
            Duration::from_secs(8),
            0.001,
        )
        .unwrap();
        p.built(1, &"0+2", Duration::from_millis(5800), 402, 0.0051)
            .unwrap();
        p.joined(2, &"0+4").unwrap();
        let spent = Spend {
            requests: 3,
            retries: 1,
            ..Spend::default()
        };
        p.end(Some(3), spent).unwrap();
        let text = String::from_utf8(out).unwrap();
        let (plan, rest) = text.split_once("Log: /m/summarise.log\n").unwrap();
        assert!(plan.starts_with("Rebuilding memory with sonnet, 8 calls at once.\n"));
        assert_eq!(
            rest,
            "[0/1]   retry 0+2        try 2 of 9 in 8s: 429 rate limited\n\
             [1/1]   built 0+2        5.8s  402 chars  $0.0051\n\
             [2/2]  joined 0+4\n\
             Built 2 summaries in 0s: 1 by the model, 1 joined. Dropped 3 summaries first.\n\
             3 calls (1 to shorten a line, 1 after a failure), $0.0000, 0 tokens.\n\
             Log: /m/summarise.log\n"
        );
    }

    #[test]
    fn nothing_to_write_prints_no_plan() {
        let mut out = Vec::new();
        let p = Progress::new(&mut out, Mode::Plain, about(Past::default()));
        assert!(!p.plan(&[level(3, 3)]).unwrap());
        p.update(&[level(3, 3)], &[], &[]).unwrap();
        assert!(!p.planned());
        drop(p);
        assert!(out.is_empty());
    }

    #[test]
    fn levels_say_where_they_stand() {
        let t = |done, left, running, waiting| Tally {
            done,
            left,
            running,
            waiting,
        };
        assert_eq!(state(&t(0, 0, 0, 0)), "nothing to write");
        assert_eq!(state(&t(3, 0, 0, 0)), "done");
        assert_eq!(state(&t(1, 4, 0, 0)), "waiting");
        assert_eq!(state(&t(1, 4, 2, 1)), "2 running · 1 retrying");
    }

    #[test]
    fn amounts_read_short() {
        assert_eq!(
            [0, 59, 843, 7500].map(|s| clock(Duration::from_secs(s))),
            ["0s", "59s", "14m 03s", "2h 05m"]
        );
        assert_eq!(
            [512, 45_600, 1_234_000].map(tokens),
            ["512", "45.6k", "1.23M"]
        );
        assert_eq!([0.00512, 2.914].map(usd), ["$0.0051", "$2.91"]);
        assert_eq!(short(&"x".repeat(200)).chars().count(), ERROR + 1);
    }
}
