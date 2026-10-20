use std::{
    collections::{HashMap, HashSet},
    fmt,
    ops::AddAssign,
    sync::{
        Arc,
        mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use backon::{BackoffBuilder, ExponentialBackoff, ExponentialBuilder};
use tracing::{Span, dispatcher, warn};

use crate::memory::{
    backend::{Backend, Block, Conversation, Usage},
    log,
    record::{Message, Who, flat},
    store::Store,
    tree::{Coord, NODE, WORDS},
    view::{Mem, VIEW},
};

pub const COMPACT: &str = include_str!("prompts/compact.txt");
pub const SCALE: &str = include_str!("prompts/scale.txt");
pub const JOBS: usize = 8;
pub const TRIES: usize = 5;
pub const RETRY: Duration = Duration::from_secs(10);
pub const RETRY_MAX: Duration = Duration::from_secs(300);
pub const RETRIES: usize = 8;

/// What one compactor call builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// One message, as `kind: text`.
    Compress(String),
    /// Two adjacent lines.
    Merge(String, String),
}

#[derive(Clone, Debug)]
pub struct Job {
    pub node: Coord,
    pub step: Step,
}

/// What every task starts with: [`SCALE`] as the size of a line.
pub fn ruler() -> String {
    format!("For scale only, this meaningless filler is exactly {WORDS} words:\n{SCALE}\n\n")
}

fn step_text(step: &Step) -> String {
    let scale = ruler();
    match step {
        Step::Compress(m) => {
            format!("{scale}Compress this message into one line, in at most {WORDS} words:\n{m}")
        }
        Step::Merge(a, b) => format!(
            "{scale}Merge these two lines into one, in at most {WORDS} words:\n{}\n{}",
            flat(a),
            flat(b)
        ),
    }
}

/// The user message of a call: the step alone, with no ids (the model would copy them).
pub fn message(job: &Job) -> Vec<Block> {
    vec![Block::new(step_text(&job.step))]
}

/// Asks for [`WORDS`] words, or fewer for a line whose words are long enough to overrun
/// [`NODE`] in fewer: as many as fill the room [`WORDS`] words take on average.
pub fn retry_text(line: &str) -> String {
    let words = line.split_whitespace().count();
    let want = (words * WORDS * 73 / 10 / line.len()).clamp(1, WORDS);
    format!(
        "That line is {words} words, too long. Rewrite it whole, as one complete line of at most {want} words: shorten the minor items, never cut it off."
    )
}

/// What the requests for lines took: all of them, and the retries among them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    pub requests: u64,
    pub retries: u64,
    pub all: Usage,
    pub retried: Usage,
}

impl Spend {
    fn add(&mut self, u: Usage, retry: bool) {
        self.requests += 1;
        self.all += u;
        if retry {
            self.retries += 1;
            self.retried += u;
        }
    }
}

impl AddAssign for Spend {
    fn add_assign(&mut self, s: Spend) {
        self.requests += s.requests;
        self.retries += s.retries;
        self.all += s.all;
        self.retried += s.retried;
    }
}

/// Builds one node with [`write_line`], adding what its requests took to `spent`. Returns who
/// wrote it, and the line.
pub fn summarize(backend: &dyn Backend, job: &Job, spent: &mut Spend) -> Result<(Who, String)> {
    write_line(backend, COMPACT, &message(job), &job.node, spent)
}

/// `conv`'s reply to `message`; what the request took goes into `spent`, failed or not.
fn ask(
    conv: &mut dyn Conversation,
    message: &[Block],
    retry: bool,
    spent: &mut Spend,
) -> Result<String> {
    let reply = conv.say(message);
    spent.add(conv.used(), retry);
    reply
}

/// The line the model writes for `message` under the system prompt `system`, retried in the
/// same conversation while over [`NODE`] bytes, up to [`TRIES`]; the shortest try wins.
/// Returns who wrote it, and the line; `node` names it in the log. What every request took
/// goes into `spent`, even when the line fails.
pub fn write_line(
    backend: &dyn Backend,
    system: &str,
    message: &[Block],
    node: &dyn fmt::Display,
    spent: &mut Spend,
) -> Result<(Who, String)> {
    let mut conv = backend.open(system)?;
    let mut reply = ask(&mut *conv, message, false, spent)?;
    let mut tries: Vec<String> = Vec::new();
    loop {
        let line = flat(reply.trim());
        if line.len() > NODE {
            let u = conv.used();
            warn!(
                node = %node,
                bytes = line.len(),
                attempt = tries.len() + 1,
                input = u.input,
                cache_write = u.cache_write,
                cache_read = u.cache_read,
                output = u.output,
                cost = log::usd(u.cost),
                "over limit"
            );
        }
        if line.is_empty() {
            bail!("empty reply");
        }
        if line.len() <= NODE || tries.len() + 1 >= TRIES {
            tries.push(line);
            break;
        }
        reply = ask(&mut *conv, &[Block::new(retry_text(&line))], true, spent)?;
        tries.push(line);
    }
    let who = Who {
        agent: backend.agent().into(),
        model: backend.model().into(),
        session: conv.session(),
    };
    let line = tries
        .into_iter()
        .min_by_key(String::len)
        .expect("one try at least");
    Ok((who, line))
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub jobs: usize,
    /// The first wait after a node fails; each next one doubles, up to [`RETRY_MAX`], plus jitter.
    pub retry: Duration,
    /// Retries of a node failing in a row before the compactor gives up ([`GaveUp`]).
    pub retries: usize,
    pub budget: u64,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            jobs: JOBS,
            retry: RETRY,
            retries: RETRIES,
            budget: VIEW,
        }
    }
}

impl Options {
    fn backoff(&self) -> ExponentialBackoff {
        ExponentialBuilder::new()
            .with_min_delay(self.retry)
            .with_max_delay(RETRY_MAX)
            .with_max_times(self.retries)
            .with_jitter()
            .build()
    }
}

/// A node failed `tries` times in a row: the compactor stops; the node stays unbuilt.
#[derive(Debug)]
pub struct GaveUp {
    pub node: Coord,
    pub tries: usize,
    pub error: String,
}

impl fmt::Display for GaveUp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let GaveUp { node, tries, error } = self;
        write!(
            f,
            "Gave up on {node} after {tries} failures in a row: {error}"
        )
    }
}

impl std::error::Error for GaveUp {}

type Done = (Coord, Duration, Spend, Result<(Who, String)>);

/// The pump: builds every node whose sources exist and whose context is all summaries,
/// up to `jobs` at once, each through `backend` on its own thread. One per memory.
pub struct Compactor<'s> {
    store: &'s Store,
    backend: Arc<dyn Backend>,
    opts: Options,
    mem: Mem,
    busy: HashSet<Coord>,
    failed: HashMap<Coord, ExponentialBackoff>,
    waits: Vec<(Instant, Coord)>,
    reports: Vec<String>,
    built: u64,
    spent: Spend,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    _lock: crate::file::Lock,
}

impl<'s> Compactor<'s> {
    /// Takes the memory's compactor lock; `None` if another compactor holds it.
    pub fn new(store: &'s Store, backend: Arc<dyn Backend>, opts: Options) -> Result<Option<Self>> {
        let Some(lock) = store.try_lock()? else {
            return Ok(None);
        };
        let mem = Mem::load(&store.snapshot()?, opts.budget)?;
        let (tx, rx) = channel();
        Ok(Some(Compactor {
            store,
            backend,
            opts,
            mem,
            busy: HashSet::new(),
            failed: HashMap::new(),
            waits: Vec::new(),
            reports: Vec::new(),
            built: 0,
            spent: Spend::default(),
            tx,
            rx,
            _lock: lock,
        }))
    }

    pub fn mem(&self) -> &Mem {
        &self.mem
    }

    /// Nodes built by the model so far.
    pub fn built(&self) -> u64 {
        self.built
    }

    /// What its requests to the model took so far.
    pub fn spent(&self) -> Spend {
        self.spent
    }

    /// The first failure of each node since it last succeeded, once.
    pub fn take_reports(&mut self) -> Vec<String> {
        std::mem::take(&mut self.reports)
    }

    /// When the earliest failed node may be tried again.
    pub fn next_retry(&self) -> Option<Instant> {
        self.waits.iter().map(|w| w.0).min()
    }

    /// Takes in what other processes wrote since, then pumps.
    pub fn refresh(&mut self) -> Result<()> {
        self.mem.absorb(&self.store.snapshot()?)?;
        self.pump()
    }

    /// Appends messages to the log and the view, then pumps.
    pub fn log(&mut self, items: &[Message]) -> Result<u64> {
        let (first, _, snap) = self.store.append("chat", items)?;
        self.mem.absorb(&snap)?;
        self.pump()?;
        Ok(first)
    }

    fn job(&self, c: Coord) -> Result<Job> {
        let step = match c.l {
            0 => Step::Compress(self.store.snapshot()?.message(c.i)?.label()),
            _ => {
                let [a, b] = c
                    .children()
                    .map(|k| self.mem.text(k).unwrap_or_default().to_string());
                Step::Merge(a, b)
            }
        };
        Ok(Job { node: c, step })
    }

    /// Starts every node that is due, up to `jobs` running.
    pub fn pump(&mut self) -> Result<()> {
        let room = self.opts.jobs.saturating_sub(self.busy.len());
        for c in self.mem.due(|c| self.busy.contains(&c), room) {
            let job = self.job(c)?;
            self.busy.insert(c);
            let (backend, tx) = (self.backend.clone(), self.tx.clone());
            let (span, dispatch) = (Span::current(), dispatcher::get_default(Clone::clone));
            thread::spawn(move || {
                let _d = dispatcher::set_default(&dispatch);
                let _s = span.enter();
                let start = Instant::now();
                let mut spent = Spend::default();
                let r = summarize(&*backend, &job, &mut spent);
                let _ = tx.send((job.node, start.elapsed(), spent, r));
            });
        }
        Ok(())
    }

    fn finish(&mut self, (c, took, spent, r): Done) -> Result<()> {
        self.spent += spent;
        match r {
            Ok((who, text)) => {
                log::spend!(info, spent, { node = %c, bytes = text.len(), secs = log::secs(took) }, "built");
                let (built, snap) = self.store.put_node(c, &who, &text)?;
                for (k, t) in built {
                    self.mem.set(k, t);
                }
                self.mem.absorb(&snap)?;
                self.mem.fit(self.mem.t());
                self.busy.remove(&c);
                self.failed.remove(&c);
                self.built += 1;
            }
            Err(e) => {
                let error = format!("{e:#}");
                let first = !self.failed.contains_key(&c);
                let backoff = self.failed.entry(c).or_insert_with(|| self.opts.backoff());
                let Some(wait) = backoff.next() else {
                    let tries = self.opts.retries + 1;
                    return Err(GaveUp {
                        node: c,
                        tries,
                        error,
                    }
                    .into());
                };
                let (secs, wait_secs) = (log::secs(took), log::secs(wait));
                if first {
                    log::spend!(warn, spent, { node = %c, error, secs, wait = wait_secs }, "failed");
                    self.reports.push(format!("{c}: {error}"));
                } else {
                    log::spend!(debug, spent, { node = %c, error, secs, wait = wait_secs }, "failed again");
                }
                self.waits.push((Instant::now() + wait, c));
            }
        }
        Ok(())
    }

    /// Waits up to `timeout` for a call to end or a failed node's wait to pass, takes in
    /// every call that ended, then pumps.
    pub fn step(&mut self, timeout: Duration) -> Result<()> {
        let wait = self.next_retry().map_or(timeout, |at| {
            at.saturating_duration_since(Instant::now()).min(timeout)
        });
        match self.rx.recv_timeout(wait) {
            Ok(done) => self.finish(done)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => unreachable!("the compactor holds a sender"),
        }
        while let Ok(done) = self.rx.try_recv() {
            self.finish(done)?;
        }
        let now = Instant::now();
        let busy = &mut self.busy;
        self.waits.retain(|&(at, c)| {
            let waiting = at > now;
            if !waiting {
                busy.remove(&c);
            }
            waiting
        });
        self.pump()
    }

    /// Runs until every line of the view is a summary.
    pub fn settle(&mut self) -> Result<()> {
        self.pump()?;
        while !self.mem.all_built() {
            if self.busy.is_empty() {
                bail!("The view has an unbuilt line and nothing is due: rule 3 broken.");
            }
            self.step(Duration::from_secs(60))?;
        }
        Ok(())
    }

    /// Runs until no node can be built on the latest snapshot; `each` after every step.
    pub fn run(&mut self, mut each: impl FnMut(&mut Self) -> Result<()>) -> Result<()> {
        loop {
            self.refresh()?;
            if self.busy.is_empty() {
                return Ok(());
            }
            while !self.busy.is_empty() {
                self.step(Duration::from_secs(60))?;
                each(self)?;
            }
        }
    }
}

impl Drop for Compactor<'_> {
    fn drop(&mut self) {
        self.backend.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_the_spec_s() {
        assert_eq!(SCALE.split_whitespace().count(), WORDS);
        assert!(SCALE.starts_with("Lorem ipsum") && SCALE.is_ascii() && !SCALE.contains('\n'));
        assert!(COMPACT.starts_with("You write the memory of ai: AI agents"));
        assert!(!COMPACT.contains("OptChat") && !COMPACT.contains("bytes"));
        assert!(COMPACT.trim_end().ends_with("Output\nonly the line."));
    }

    #[test]
    fn steps_carry_no_ids_and_no_context() {
        let job = Job {
            node: Coord::new(1, 0),
            step: Step::Merge("user: hi".into(), "ai: a\nb".into()),
        };
        let m = message(&job);
        assert_eq!(m.len(), 1, "the step alone: no context");
        assert!(m[0].text.starts_with(&format!(
            "For scale only, this meaningless filler is exactly 50 words:\n{SCALE}\n\n"
        )));
        assert!(
            m[0].text.ends_with(
                "Merge these two lines into one, in at most 50 words:\nuser: hi\nai: a b"
            )
        );
        let c = step_text(&Step::Compress("tool: x\ny".into()));
        assert!(
            c.ends_with("Compress this message into one line, in at most 50 words:\ntool: x\ny")
        );
        assert!(!c.contains("bytes"));
    }

    #[test]
    fn retries_ask_for_the_target_in_words() {
        let r = retry_text(&"word ".repeat(104));
        assert!(r.starts_with(
            "That line is 104 words, too long. Rewrite it whole, as one complete line of at most 50 words:"
        ));
        assert!(r.ends_with("never cut it off.") && !r.contains("bytes"));
        let dense = retry_text("0123456789ab ".repeat(45).trim_end());
        assert!(dense.contains("at most 28 words"), "{dense}");
    }
}
