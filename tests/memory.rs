use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use ai_tools::memory::{
    Backend, Block, Cli, Compactor, Conversation, Coord, Field, GaveUp, HIDDEN, Kind, LONG_PROMPT,
    Mem, Message, Meta, NODE, Options, PLACEHOLDER, Place, REMIND_EVERY, REMINDER, Runtime, Store,
    Usage, VIEW, Who, zoom,
};
use anyhow::{Result, bail};
use clap::Parser;
use regex::Regex;
use tempfile::TempDir;

const AWAKE: &str = "You are awake. Zoom the marked lines that touch your task before you act.";

/// The `(id, n)` of every line printed in `out`.
fn line_ids(out: &str) -> Vec<(u64, u64)> {
    out.lines()
        .filter_map(|l| Coord::parse(l.split_once('|')?.0))
        .collect()
}

/// Whether `ids` tile `[from, to)` in order.
fn tiles(ids: &[(u64, u64)], from: u64, to: u64) -> bool {
    ids.first().is_some_and(|f| f.0 == from)
        && ids.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0)
        && ids.last().is_some_and(|l| l.0 + l.1 == to)
}

fn is_day(l: &str) -> bool {
    l.len() == 10 && l.parse::<jiff::civil::Date>().is_ok()
}

struct Out {
    code: u8,
    stdout: String,
    stderr: String,
}

fn runtime(backend: Arc<dyn Backend>) -> Runtime {
    Runtime {
        backend: Box::new(move |_| Ok(backend.clone())),
        terminal: false,
        opts: opts(VIEW, 8),
        summarise_on_note: false,
        before_release: Box::new(|| {}),
        stdin: Box::new(|| Ok(String::new())),
    }
}

fn run_rt(dir: &Path, rt: &Runtime, args: &[&str]) -> Out {
    let cli = match Cli::try_parse_from(std::iter::once("memory").chain(args.iter().copied())) {
        Ok(cli) => cli,
        Err(e) => {
            return Out {
                code: 2,
                stdout: String::new(),
                stderr: e.to_string(),
            };
        }
    };
    let mut buf = Vec::new();
    let result = cli.run_with(dir, &mut buf, rt);
    let stdout = String::from_utf8(buf).unwrap();
    match result {
        Ok(c) => Out {
            code: u8::from(c != ExitCode::SUCCESS),
            stdout,
            stderr: String::new(),
        },
        Err(e) => Out {
            code: 1,
            stdout,
            stderr: format!("{e:#}"),
        },
    }
}

fn run(dir: &Path, args: &[&str]) -> Out {
    run_rt(
        dir,
        &runtime(Arc::new(Model(Arc::new(Fake::summaries())))),
        args,
    )
}

fn hook(dir: &Path, input: &str) -> Out {
    let input = input.to_string();
    let rt = Runtime {
        stdin: Box::new(move || Ok(input.clone())),
        ..runtime(Arc::new(Model(Arc::new(Fake::summaries()))))
    };
    run_rt(dir, &rt, &["prompt-hook"])
}

fn store() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path().join("memory");
    assert_eq!(run(&d, &["init"]).code, 0);
    (tmp, d)
}

fn msg(kind: Kind, text: impl Into<String>) -> Message {
    Message::new(kind, &text.into())
}

fn long(i: u64) -> String {
    format!("message {i} {}", "x".repeat(600))
}

/// A model that answers each call with `reply(step block, attempt)`.
type Reply = dyn Fn(&str, usize) -> Result<String> + Send + Sync;

struct Fake {
    reply: Box<Reply>,
    calls: AtomicUsize,
    says: AtomicUsize,
    running: AtomicUsize,
    peak: AtomicUsize,
    log: Mutex<Vec<String>>,
}

impl Fake {
    fn new(reply: impl Fn(&str, usize) -> Result<String> + Send + Sync + 'static) -> Fake {
        Fake {
            reply: Box::new(reply),
            calls: AtomicUsize::new(0),
            says: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            log: Mutex::default(),
        }
    }

    /// A deterministic summary of 300 bytes naming what it summarized: two never merge free.
    fn summaries() -> Fake {
        Fake::new(|step, _| Ok(format!("{:.<300}", tag(step))))
    }
}

/// What a step summarizes: `m<i>` for message i, `join` otherwise.
fn tag(step: &str) -> String {
    let re = Regex::new(r"\n\w+: message (\d+) ").unwrap();
    match re.captures(step) {
        Some(c) if step.contains("Compress this message") => format!("m{}", &c[1]),
        _ => "join".into(),
    }
}

struct FakeConv {
    fake: Arc<Fake>,
    step: String,
    attempt: usize,
}

struct Model(Arc<Fake>);

fn fake_who() -> Who {
    Who {
        agent: "fake-harness".into(),
        model: "fake".into(),
        session: "fake-session".into(),
    }
}

impl Backend for Model {
    fn agent(&self) -> &str {
        "fake-harness"
    }

    fn model(&self) -> &str {
        "fake"
    }

    fn open(&self, system: &str) -> Result<Box<dyn Conversation>> {
        assert!(system.starts_with("You write the memory of ai"));
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeConv {
            fake: self.0.clone(),
            step: String::new(),
            attempt: 0,
        }))
    }
}

impl Conversation for FakeConv {
    fn say(&mut self, message: &[Block]) -> Result<String> {
        let f = &self.fake;
        f.says.fetch_add(1, Ordering::SeqCst);
        if self.attempt == 0 {
            let [step] = message else {
                panic!("a call is the step alone, without context: {message:?}");
            };
            let ids = Regex::new(r"(?m)^\d+\+\d+\|").unwrap();
            assert!(
                !ids.is_match(&step.text) && !step.text.contains(PLACEHOLDER),
                "{}",
                step.text
            );
            self.step = step.text.clone();
            f.log.lock().unwrap().push(tag(&self.step));
        } else {
            assert_eq!(message.len(), 1);
            assert!(message[0].text.ends_with("never cut it off."));
        }
        self.attempt += 1;
        let now = f.running.fetch_add(1, Ordering::SeqCst) + 1;
        f.peak.fetch_max(now, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(2));
        f.running.fetch_sub(1, Ordering::SeqCst);
        (f.reply)(&self.step, self.attempt)
    }

    fn session(&self) -> String {
        "fake-session".into()
    }

    /// Every request: 100 tokens in, 10 out, a cent.
    fn used(&self) -> Usage {
        Usage {
            input: 100,
            output: 10,
            cost: 0.01,
            ..Usage::default()
        }
    }
}

fn compactor<'s>(s: &'s Store, fake: &Arc<Fake>, opts: Options) -> Compactor<'s> {
    Compactor::new(s, Arc::new(Model(fake.clone())), opts)
        .unwrap()
        .expect("the lock is free")
}

fn opts(budget: u64, jobs: usize) -> Options {
    Options {
        jobs,
        retry: Duration::from_millis(20),
        retries: 3,
        budget,
    }
}

/// Every view part in order: tiles `[0, t)`.
fn check_tiles(m: &Mem) {
    let mut at = 0;
    for c in m.view() {
        assert_eq!(c.id(), at, "gap or overlap at {at}");
        at = c.end();
    }
    assert_eq!(at, m.t());
}

fn mergeable(m: &Mem) -> bool {
    m.view().windows(2).any(|w| {
        let (a, b) = (w[0], w[1]);
        a.l == b.l && a.i % 2 == 0 && b.i == a.i + 1 && m.built(a.parent())
    })
}

#[test]
fn memory_alone_prints_the_view() {
    let (_tmp, d) = store();
    assert_eq!(run(&d, &["note", "the first note"]).code, 0);
    let view = run(&d, &["view"]);
    assert_eq!(view.code, 0, "{}", view.stderr);
    assert!(view.stdout.contains("the first note"), "{}", view.stdout);
    assert_eq!(run(&d, &[]).stdout, view.stdout);
}

#[test]
fn the_view_folds_incrementally() {
    let mut m = Mem::new(6000);
    let mut seed = 12345u64;
    let mut rand = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed >> 33
    };
    let mut prev = BTreeSet::new();
    let mut merges = 0;
    for i in 0..1200u64 {
        m.add_message();
        let short = rand() % 10 < 3;
        loop {
            let due = m.due(|_| false, usize::MAX);
            if due.is_empty() {
                break;
            }
            for c in due {
                let n = if c.l == 0 && short {
                    40
                } else {
                    150 + (rand() % 60) as usize
                };
                m.add_node(c, "y".repeat(n));
            }
        }
        check_tiles(&m);
        assert!(m.all_built(), "after {i}");
        let size: u64 = m
            .view()
            .iter()
            .map(|&c| m.text(c).unwrap().len() as u64)
            .sum();
        assert_eq!(size, m.size());
        assert!(
            size <= m.budget() || !mergeable(&m),
            "over budget with a mergeable pair"
        );
        let bounds: BTreeSet<u64> = m.view().iter().map(|c| c.id()).collect();
        let split = bounds.iter().any(|b| *b < i && !prev.contains(b));
        assert!(!split, "a part was split after message {i}");
        if bounds.len() < prev.len() + 1 {
            merges += 1;
        }
        prev = bounds;
        if i % 100 == 99 {
            let mut copy = m.clone();
            copy.refold();
            assert_eq!(copy.view(), m.view(), "refold differs after {i}");
        }
    }
    assert!(merges > 100, "{merges} merges");
    assert!(m.view().len() < 80, "{} lines", m.view().len());
    assert!(
        m.size() > 6000 - 400,
        "the view stays near its budget: {}",
        m.size()
    );
}

#[test]
fn due_is_every_ready_node_oldest_level_first() {
    let mut m = Mem::new(VIEW);
    for _ in 0..6 {
        m.add_message();
    }
    let leaves = |is: &[u64]| is.iter().map(|&i| Coord::leaf(i)).collect::<Vec<_>>();
    assert_eq!(m.due(|_| false, 10), leaves(&[0, 1, 2, 3, 4, 5]));
    m.add_node(Coord::leaf(0), "a".into());
    m.add_node(Coord::leaf(1), "b".into());
    let mut want = leaves(&[2, 3, 4, 5]);
    want.push(Coord::new(1, 0));
    assert_eq!(m.due(|_| false, 10), want);
    assert_eq!(m.due(|c| c.l == 0, 10), [Coord::new(1, 0)]);
    assert_eq!(m.due(|_| false, 1), [Coord::leaf(2)]);
    m.add_node(Coord::leaf(3), "d".into());
    let mut want = leaves(&[2, 4, 5]);
    want.push(Coord::new(1, 0));
    assert_eq!(m.due(|_| false, 10), want);
    assert_eq!(m.lines()[2..4], [format!("2|{PLACEHOLDER}"), "3|d".into()]);
    assert!(m.render().starts_with("<chat>\n0|a\n1|b\n") && m.render().ends_with("\n</chat>"));
}

#[test]
fn short_messages_are_their_own_nodes() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    let (first, built, snap) = s
        .append(
            "t",
            &[
                msg(Kind::User, "hi\nthere"),
                msg(Kind::Ai, "hello"),
                msg(Kind::Tool, "Bash {\"command\":\"ls\"}\nsrc"),
            ],
        )
        .unwrap();
    assert_eq!(first, 0);
    let names: Vec<_> = built.iter().map(|(c, t)| format!("{c}={t}")).collect();
    assert_eq!(
        names,
        [
            "0=user: hi there",
            "1=ai: hello",
            "0+2=user: hi there ai: hello",
            "2="
        ],
        "a tool call's line is empty"
    );
    let free = snap.node(Coord::new(1, 0)).unwrap().unwrap();
    assert_eq!(free.who, Who::default());
    assert_eq!(free.origin, s.origin());
    let exact = "x".repeat(NODE);
    let line = exact.clone();
    let (_, built, snap) = s.append("t", &[msg(Kind::Note, exact.clone())]).unwrap();
    let names: Vec<_> = built.iter().map(|(c, t)| format!("{c}={t}")).collect();
    assert_eq!(
        names,
        [format!("3={line}"), format!("2+2={line}")],
        "a merge with tool calls only is its other side"
    );
    assert!(
        snap.node(Coord::new(2, 0)).unwrap().is_none(),
        "the merge of a long line and a full one is not free"
    );
    assert_eq!(
        zoom(&snap, 2, 2, None).unwrap().unwrap(),
        format!("2|{HIDDEN}\n3|{line}")
    );
    let (built, _) = s.put_node(Coord::new(2, 0), &fake_who(), "merged").unwrap();
    assert_eq!(built.len(), 1);
    let (again, _) = s.put_node(Coord::new(2, 0), &fake_who(), "other").unwrap();
    assert!(again.is_empty(), "built nodes never change");
    assert_eq!(
        zoom(&s.snapshot().unwrap(), 0, 4, None).unwrap().unwrap(),
        format!("0|user: hi there\n1|ai: hello\n2|{HIDDEN}\n3|{line}"),
        "a line over a free half and a merged half opens into all four messages"
    );
    assert!(s.put_node(Coord::new(3, 0), &fake_who(), "x").is_err());
    let m = Mem::load(&s.snapshot().unwrap(), VIEW).unwrap();
    assert_eq!(m.t(), 4);
    assert!(m.all_built());
    let ids: Vec<_> = m
        .lines()
        .iter()
        .map(|l| l.split('|').next().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["0", "1", "3"], "the view leaves out tool calls");
}

#[test]
fn zoom_opens_two_levels() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    let items: Vec<_> = (0..16)
        .map(|i| msg(Kind::User, format!("w{i}\nm")))
        .collect();
    s.append("t", &items).unwrap();
    s.append("t", &[msg(Kind::Ai, long(16))]).unwrap();
    let snap = s.snapshot().unwrap();
    let z = |id, n| zoom(&snap, id, n, None).unwrap();
    let pair = |i: u64| format!("user: w{i} m user: w{} m", i + 1);
    let four = |i: u64| format!("{} {}", pair(i), pair(i + 2));
    assert_eq!(z(1, 1).unwrap(), "1|user: w1\nm");
    assert_eq!(z(16, 1).unwrap(), format!("16|ai: {}", long(16)));
    assert_eq!(
        z(0, 16).unwrap(),
        format!(
            "0+4|{}\n4+4|{}\n8+4|{}\n12+4|{}",
            four(0),
            four(4),
            four(8),
            four(12)
        ),
        "a deep line opens into its four grandchildren"
    );
    assert_eq!(
        z(8, 8).unwrap(),
        format!(
            "8+2|{}\n10+2|{}\n12+2|{}\n14+2|{}",
            pair(8),
            pair(10),
            pair(12),
            pair(14)
        )
    );
    assert_eq!(
        z(4, 4).unwrap(),
        "4|user: w4 m\n5|user: w5 m\n6|user: w6 m\n7|user: w7 m",
        "+4 opens into its messages"
    );
    assert_eq!(
        z(0, 2).unwrap(),
        "0|user: w0 m\n1|user: w1 m",
        "+2 opens into its two messages"
    );
    assert_eq!(
        z(16, 2),
        None,
        "16+2 is not built: message 17 has no line yet"
    );
    assert_eq!(z(3, 2), None);
    assert_eq!(z(0, 32), None);
    assert_eq!(z(17, 1), None);
    assert_eq!(z(0, 3), None, "+3 is not a line");
    let meta = Meta {
        time: false,
        fields: vec![Field::Kind, Field::Agent],
    };
    let ts = snap.message(0).unwrap().ts;
    let day = format!("{}-{}-{}", &ts[..4], &ts[4..6], &ts[6..8]);
    assert_eq!(
        zoom(&snap, 0, 2, Some(&meta)).unwrap().unwrap(),
        format!("{day}\n0|user|-|user: w0 m\n1|user|-|user: w1 m")
    );
    assert_eq!(
        zoom(&snap, 16, 1, Some(&Meta::default())).unwrap().unwrap(),
        format!("{day}\n16|ai: {}", long(16))
    );
}

#[test]
fn the_compactor_builds_everything_and_settles() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    let items: Vec<_> = (0..12).map(|i| msg(Kind::User, long(i))).collect();
    s.append("t", &items).unwrap();
    let fake = Arc::new(Fake::summaries());
    let mut c = compactor(&s, &fake, opts(VIEW, 3));
    c.settle().unwrap();
    assert!(c.mem().all_built());
    c.run(|_| Ok(())).unwrap();
    let log = fake.log.lock().unwrap().clone();
    let mut leaves: Vec<_> = log.iter().filter(|t| t.starts_with('m')).cloned().collect();
    leaves.sort_by_key(|t| t[1..].parse::<u64>().unwrap());
    assert_eq!(
        leaves,
        (0..12).map(|i| format!("m{i}")).collect::<Vec<_>>(),
        "every message once"
    );
    assert_eq!(log.len(), 12 + 6 + 3 + 1);
    let peak = fake.peak.load(Ordering::SeqCst);
    assert!((2..=3).contains(&peak), "peak {peak}");
    assert_eq!(c.built(), 22);
    let snap = s.snapshot().unwrap();
    assert!(snap.node(Coord::new(3, 0)).unwrap().is_some());
    assert!(snap.node(Coord::new(4, 0)).unwrap().is_none());
    let leaf = snap.node(Coord::leaf(0)).unwrap().unwrap();
    assert_eq!((leaf.who, leaf.origin.as_str()), (fake_who(), s.origin()));
    let loaded = Mem::load(&snap, VIEW).unwrap();
    for l in 0..4 {
        for i in 0..12 >> l {
            let k = Coord::new(l, i);
            assert_eq!(loaded.text(k), c.mem().text(k), "{k}");
        }
    }
    assert!(
        Compactor::new(&s, Arc::new(Model(fake.clone())), Options::default())
            .unwrap()
            .is_none()
    );
    drop(c);
    assert!(
        Compactor::new(&s, Arc::new(Model(fake.clone())), Options::default())
            .unwrap()
            .is_some()
    );
}

#[test]
fn the_compactor_folds_the_view_under_its_budget() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    let fake = Arc::new(Fake::summaries());
    let mut c = compactor(&s, &fake, opts(3000, 8));
    for i in 0..100 {
        c.log(&[msg(Kind::Ai, long(i))]).unwrap();
        c.settle().unwrap();
        check_tiles(c.mem());
        assert!(c.mem().size() <= 3000 || !mergeable(c.mem()));
    }
    assert!(c.mem().view().len() < 30);
    assert_eq!(
        fake.log
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.starts_with('m'))
            .count(),
        100
    );
}

#[test]
fn notes_from_elsewhere_are_picked_up() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    s.append("t", &[msg(Kind::User, long(0))]).unwrap();
    let fake = Arc::new(Fake::summaries());
    let mut c = compactor(&s, &fake, opts(VIEW, 8));
    let other = Store::open(&d).unwrap();
    other.append("t", &[msg(Kind::Note, long(1))]).unwrap();
    c.run(|_| Ok(())).unwrap();
    assert_eq!(c.mem().t(), 2);
    assert!(c.mem().built(Coord::new(1, 0)));
}

#[test]
fn overshoots_are_retried_in_the_same_conversation() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    s.append("t", &[msg(Kind::User, long(0))]).unwrap();
    let fake = Arc::new(Fake::new(|_, attempt| {
        Ok(format!("  {}  ", "z".repeat(600 - 40 * attempt)))
    }));
    let mut c = compactor(&s, &fake, opts(VIEW, 8));
    c.run(|_| Ok(())).unwrap();
    assert_eq!(
        (
            fake.calls.load(Ordering::SeqCst),
            fake.says.load(Ordering::SeqCst)
        ),
        (1, 3)
    );
    assert_eq!(c.mem().text(Coord::leaf(0)).unwrap().len(), 480);

    s.append("t", &[msg(Kind::User, long(1))]).unwrap();
    let stubborn = Arc::new(Fake::new(|_, attempt| Ok("w".repeat(700 - attempt))));
    drop(c);
    let mut c = compactor(&s, &stubborn, opts(VIEW, 8));
    c.run(|_| Ok(())).unwrap();
    assert_eq!(
        stubborn.calls.load(Ordering::SeqCst),
        2,
        "message 1, then 0+2"
    );
    assert_eq!(stubborn.says.load(Ordering::SeqCst), 10, "TRIES each");
    assert_eq!(
        c.mem().text(Coord::leaf(1)).unwrap().len(),
        695,
        "the shortest try"
    );
}

#[test]
fn failures_are_retried_and_reported_once() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    s.append("t", &[msg(Kind::User, long(0))]).unwrap();
    let left = Arc::new(Mutex::new(3));
    let l2 = left.clone();
    let fake = Arc::new(Fake::new(move |_, _| {
        let mut n = l2.lock().unwrap();
        if *n > 0 {
            *n -= 1;
            bail!("overloaded");
        }
        Ok("a summary".into())
    }));
    let mut c = compactor(&s, &fake, opts(VIEW, 8));
    let mut reports = Vec::new();
    c.run(|c| {
        reports.extend(c.take_reports());
        Ok(())
    })
    .unwrap();
    assert_eq!(reports, ["0: overloaded"]);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 4);
    assert_eq!(c.mem().text(Coord::leaf(0)), Some("a summary"));
    let blanks = AtomicUsize::new(0);
    let empty = Arc::new(Fake::new(move |_, _| {
        let n = blanks.fetch_add(1, Ordering::SeqCst);
        Ok(if n < 2 { "  ".into() } else { "x".into() })
    }));
    s.append("t", &[msg(Kind::User, long(1))]).unwrap();
    drop(c);
    let mut c = compactor(&s, &empty, opts(VIEW, 8));
    c.run(|_| Ok(())).unwrap();
    assert_eq!(c.take_reports(), ["1: empty reply"]);
}

#[test]
fn a_node_failing_past_its_retries_stops_the_compactor() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    s.append("t", &[msg(Kind::User, long(0))]).unwrap();
    let dead = Arc::new(Fake::new(|_, _| bail!("logged out")));
    let mut c = compactor(&s, &dead, opts(VIEW, 8));
    let e = c.run(|_| Ok(())).unwrap_err();
    let g = e.downcast_ref::<GaveUp>().expect("gave up");
    assert_eq!(
        (g.node, g.tries, g.error.as_str()),
        (Coord::leaf(0), 4, "logged out")
    );
    assert_eq!(
        dead.calls.load(Ordering::SeqCst),
        4,
        "the first try and 3 retries"
    );
    drop(c);
    let fine = Arc::new(Fake::summaries());
    let mut c = compactor(&s, &fine, opts(VIEW, 8));
    c.run(|_| Ok(())).unwrap();
    assert!(c.mem().built(Coord::leaf(0)));
}

#[test]
fn a_summariser_on_a_dead_backend_gives_up_and_frees_the_lock() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    s.append("t", &[msg(Kind::User, long(0)), msg(Kind::User, long(1))])
        .unwrap();
    let dead = Fake::new(|_, _| bail!("usage limit reached"));
    let r = run_rt(&d, &runtime(Arc::new(Model(Arc::new(dead)))), &["build"]);
    assert_eq!(r.code, 1, "{}", r.stderr);
    let failed = |n| {
        format!(
            "Failed {n}: usage limit reached. Retrying up to 3 times, waiting longer each time.\n"
        )
    };
    assert!(
        r.stdout.starts_with(&failed(0)) || r.stdout.starts_with(&failed(1)),
        "{}",
        r.stdout
    );
    assert!(
        Regex::new(r"\nGave up on [01] after 4 failures in a row: usage limit reached\. It stays pending; the summariser after the next note tries again\.\n$")
            .unwrap()
            .is_match(&r.stdout),
        "{}",
        r.stdout
    );
    let log = fs::read_to_string(d.join("summarise.log")).unwrap();
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR ")).collect();
    assert_eq!(errors.len(), 1, "{log}");
    assert!(
        Regex::new(r#": gave up node=[01] tries=4 error="usage limit reached" requests=\d+ retries=0 tokens=\d+ cost=[\d.]+ retry_tokens=0 retry_cost=0.0$"#)
            .unwrap()
            .is_match(errors[0]),
        "{log}"
    );
    assert!(!log.contains(" end "), "{log}");
    assert!(!Mem::load(&s.snapshot().unwrap(), VIEW).unwrap().all_built());
    assert_eq!(run(&d, &["build"]).stdout, "Built 3 summaries.\n");
}

#[test]
fn a_life_through_the_cli() {
    let (tmp, d) = store();
    let d = d.as_path();
    let r = run(d, &["note", &"x".repeat(507)]);
    assert!(r.code == 1 && r.stderr.contains("Too long"), "{}", r.stderr);
    let r = run(d, &["note", "two\nlines"]);
    assert!(r.code == 1 && r.stderr.contains("one line"));
    assert_eq!(run(d, &["note", "   "]).code, 1);
    let r = run(d, &["view"]);
    assert!(r.stdout.contains("No memories yet") && r.stdout.ends_with("You are awake.\n"));
    assert_eq!(run(d, &["build"]).stdout, "Nothing to build.\n");

    let seed = tmp.path().join("seed.txt");
    let lines: String = (0..40)
        .map(|i| format!("2020-01-{:02} message {i} {}\n", 1 + i / 2, "n".repeat(300)))
        .collect();
    fs::write(&seed, lines).unwrap();
    let r = run(d, &["import", seed.to_str().unwrap()]);
    assert_eq!(r.stdout, "Imported 40 notes, 0 to 39.\n", "{}", r.stderr);
    let r = run(d, &["note", "a short one"]);
    assert_eq!(r.stdout, "Saved as 40.\n");
    let r = run(d, &["note", &format!("big {}", "b".repeat(270))]);
    assert_eq!(r.stdout, "Saved as 41.\n");

    let view = run(d, &["view"]).stdout;
    let ts = Store::open(d)
        .unwrap()
        .snapshot()
        .unwrap()
        .message(40)
        .unwrap()
        .ts;
    let (today, now) = (
        format!("{}-{}-{}", &ts[..4], &ts[4..6], &ts[6..8]),
        format!("{}:{}", &ts[9..11], &ts[11..13]),
    );
    assert!(view.starts_with("2020-01-01\n0|message 0 nnn"), "{view}");
    assert!(
        view.contains(&format!("\n{today}\n40|a short one\n")),
        "{view}"
    );
    let view = run(d, &["view", "--time", "-o", "kind,repo"]).stdout;
    assert!(
        view.starts_with("2020-01-01\n0|00:00|note|-|message 0 nnn"),
        "{view}"
    );
    assert!(
        view.contains(&format!("\n{today}\n40|{now}|note|")),
        "{view}"
    );
    assert!(view.ends_with(&format!("\n{AWAKE}\n")), "{view}");

    let rt = runtime(Arc::new(Model(Arc::new(Fake::summaries()))));
    let r = run_rt(d, &rt, &["build"]);
    assert!(
        Regex::new(r"^Built \d+ summaries\.\n$")
            .unwrap()
            .is_match(&r.stdout),
        "{}{}",
        r.stdout,
        r.stderr
    );
    let s = Store::open(d).unwrap();
    let snap = s.snapshot().unwrap();
    for l in 0..6 {
        for i in 0..42u64 >> l {
            assert!(
                snap.node(Coord::new(l, i)).unwrap().is_some(),
                "{}",
                Coord::new(l, i)
            );
        }
    }
    assert_eq!(run(d, &["build"]).stdout, "Nothing to build.\n");

    assert_eq!(run(d, &["config", "WAKE_LINES=8"]).code, 0);
    let view = run(d, &["view"]).stdout;
    let ids = line_ids(&view);
    assert_eq!(ids.len(), 8, "{view}");
    assert!(
        ids[0].0 == 0 && ids[0].1 > 1,
        "the oldest are summarized: {view}"
    );
    assert!(ids.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0), "{view}");
    assert_eq!(ids[7].0 + ids[7].1, 42, "{view}");
    assert_eq!(run(d, &["config", "WAKE_LINES="]).code, 0);

    let one = run(d, &["view"]).stdout;
    assert!(is_day(one.lines().next().unwrap()), "{one}");
    assert!(
        one.ends_with(&format!("\n{AWAKE}\n")),
        "a short view is one part, with no footer: {one}"
    );
    let mark = Store::open(d)
        .unwrap()
        .snapshot()
        .unwrap()
        .mark()
        .to_string();
    run(d, &["note", "lands after the read"]);
    assert_eq!(run(d, &["view", "--at", &mark]).stdout, one);
    let whole = run(d, &["view", "--whole"]).stdout;
    assert!(whole.contains("\n42|lands after the read\n"), "{whole}");
    assert!(tiles(&line_ids(&whole), 0, 43), "{whole}");
    assert_eq!(
        run(d, &["view", "--part", "2"]).stderr,
        "No part 2: it has 1."
    );
    assert!(
        run(d, &["view", "--at", "xyz"])
            .stderr
            .contains("'xyz' is not a memory state")
    );
    let unknown = "0".repeat(40);
    assert!(
        run(d, &["view", "--at", &unknown])
            .stderr
            .contains("is not a state of this memory")
    );

    let z = run(d, &["zoom", "0+32"]).stdout;
    assert_eq!(z.lines().filter(|l| l.contains('|')).count(), 4, "{z}");
    assert!(
        z.starts_with("2020-01-01\n0+8|..01-04|") && z.contains("\n2020-01-13\n24+8|..01-16|"),
        "{z}"
    );
    assert!(
        run(d, &["zoom", "3"])
            .stdout
            .starts_with("2020-01-02\n3|message 3 n")
    );
    let leaves = run(d, &["zoom", "0+4"]).stdout;
    for l in leaves.lines().filter(|l| l.contains('|')) {
        let id = l.split_once('|').unwrap().0;
        assert!(!id.contains('+'), "one message prints a bare id: {l}");
        let z = run(d, &["zoom", id]).stdout;
        assert!(
            z.lines().nth(1).unwrap().starts_with(&format!("{id}|")),
            "{z}"
        );
    }
    let z = run(d, &["zoom", "32+8", "-o", "repo"]).stdout;
    assert_eq!(z.lines().filter(|l| l.contains('|')).count(), 4, "{z}");
    assert!(z.starts_with("2020-01-17\n32+2|-|"), "{z}");
    assert!(z.contains("\n2020-01-20\n38+2|-|"), "{z}");
    assert_eq!(run(d, &["zoom", "0+64"]).stderr, "No line 0+64.");
    assert_eq!(run(d, &["zoom", "1+2"]).stderr, "No line 1+2.");
    assert!(run(d, &["zoom", "x"]).stderr.contains("not a line id"));
    assert!(
        run(d, &["zoom", "3", "--part", "99"])
            .stderr
            .contains("No part 99")
    );

    let origin = Store::open(d).unwrap().origin().to_string();
    let show = run(d, &["show", "40"]).stdout;
    assert!(show.starts_with("40\nts      "));
    assert!(show.contains("\nkind    note\n") && show.contains("\ntext    a short one\n"));
    assert!(show.contains(&format!("\nkind    note\norigin  {origin}\nrepo    ")));
    let show = run(d, &["show", "0+2"]).stdout;
    assert!(
        show.contains(&format!(
            "\norigin  {origin}\nagent   fake-harness\nmodel   fake\nsession fake-session\ntext    join."
        )),
        "{show}"
    );

    let g = run(d, &["grep", "message 3\\b", "--time", "-o", "origin,repo"]).stdout;
    assert!(
        g.starts_with(&format!("2020-01-02\n3|00:00|{origin}|-|message 3 nnn")),
        "{g}"
    );
    let n = |args: &[&str]| run(d, args).stdout;
    assert_eq!(
        n(&["grep", "message 3\\b", "--origin", "NOPE"]),
        "No match.\n"
    );
    assert_eq!(
        n(&[
            "grep",
            "message 3\\b",
            "--origin",
            &origin.to_lowercase(),
            "-c"
        ]),
        "1 match.\n"
    );
    let all = n(&["grep", "^join", "-t", "-c"]);
    assert_eq!(
        n(&["grep", "^join", "-t", "-c", "--agent", "fake-harness"]),
        all
    );
    assert_eq!(
        n(&["grep", "^join", "-t", "-c", "--session", "fake-session"]),
        all
    );
    assert_eq!(n(&["grep", "^join", "-t", "-c", "--origin", &origin]), all);
    assert_eq!(
        n(&["grep", "^join", "-t", "--agent", "other"]),
        "No match.\n"
    );
    assert!(g.ends_with("\n1 match.\n"));
    let g = run(d, &["grep", "^join", "-t", "-c"]).stdout;
    assert!(
        Regex::new(r"^\d+ matches\.\n$").unwrap().is_match(&g),
        "{g}"
    );
    assert_eq!(
        run(d, &["grep", "join", "-t", "--kind", "note"]).stdout,
        "No match.\n"
    );
    let g = run(d, &["grep", "message", "-m", "5"]).stdout;
    let ids: Vec<&str> = g
        .lines()
        .filter_map(|l| l.split_once('|'))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, ["35", "36", "37", "38", "39"], "{g}");
    assert_eq!(g.lines().last().unwrap(), "Newest 5 of 40 matches.");
    let g = run(d, &["grep", "big"]).stdout;
    assert!(g.starts_with(&format!("{today}\n41|big bbb")), "{g}");
    assert_eq!(run(d, &["grep", "^"]).code, 0);

    let bad = tmp.path().join("bad.txt");
    fs::write(&bad, "2019-01-01 too old\n").unwrap();
    assert!(
        run(d, &["import", bad.to_str().unwrap()])
            .stderr
            .contains("the log is in time order")
    );
    fs::write(&bad, "2099-01-02 later\n2099-01-01 back\n").unwrap();
    assert!(
        run(d, &["import", bad.to_str().unwrap()])
            .stderr
            .contains("line 2: date 2099-01-01 precedes")
    );
    fs::write(&bad, "2021-02-30 no such day\n").unwrap();
    assert!(
        run(d, &["import", bad.to_str().unwrap()])
            .stderr
            .contains("not a real date")
    );
}

#[test]
fn long_output_comes_in_parts_of_one_state_and_whole_on_a_terminal() {
    let (tmp, d) = store();
    let d = &d;
    let seed = tmp.path().join("seed.txt");
    let big = format!("big {}", "y ".repeat(15_000))
        .trim_end()
        .to_string();
    let notes: String = (0..80)
        .map(|k| format!("2020-01-01 note {k} {}\n", "x".repeat(260)))
        .chain([format!("2020-01-02 {big}\n")])
        .collect();
    fs::write(&seed, notes).unwrap();
    assert_eq!(run(d, &["import", seed.to_str().unwrap()]).code, 0);

    let first = run(d, &["view", "--time"]).stdout;
    let (body, footer) = first.trim_end().rsplit_once('\n').unwrap();
    let next = footer
        .strip_prefix("Part 1 of 2. Next: ai memory ")
        .unwrap();
    assert!(
        Regex::new(r"^view --part 2 --at [0-9a-f]{40} --time$")
            .unwrap()
            .is_match(next),
        "{next}"
    );
    run(d, &["note", "lands between two parts"]);
    let args: Vec<&str> = next.split(' ').collect();
    let second = run(d, &args).stdout;
    let (rest, footer) = second.trim_end().rsplit_once('\n').unwrap();
    assert_eq!(footer, "Part 2 of 2.");
    let whole = run(d, &["view", "--whole", "--at", args[4], "--time"]).stdout;
    assert_eq!(
        format!("{body}\n{rest}\n"),
        whole,
        "the parts come from one state"
    );
    assert!(!whole.contains("lands between"), "{whole}");
    assert!(whole.ends_with(&format!("\n{AWAKE}\n")), "{whole}");

    let terminal = Runtime {
        terminal: true,
        ..runtime(Arc::new(Model(Arc::new(Fake::summaries()))))
    };
    let shown = run_rt(d, &terminal, &["view"]).stdout;
    assert!(
        shown.contains("\n81|lands between two parts\n")
            && shown.ends_with(&format!("\n{AWAKE}\n")),
        "a terminal reads the latest whole: {shown}"
    );
    assert!(
        run_rt(d, &terminal, &["view", "--part", "1"])
            .stdout
            .contains("\nPart 1 of 2. Next: ai memory view --part 2 --at ")
    );

    let first = run(d, &["zoom", "80"]).stdout;
    let (body, next) = first.split_once("\nPart 1 of 2. Next: ai memory ").unwrap();
    assert!(
        Regex::new(r"^zoom 80 --part 2 --at [0-9a-f]{40}\n$")
            .unwrap()
            .is_match(next),
        "{next}"
    );
    let rest = run(d, &next.split_whitespace().collect::<Vec<_>>()).stdout;
    let rest = rest.strip_suffix("Part 2 of 2.\n").unwrap();
    assert_eq!(format!("{body}{rest}"), format!("2020-01-02\n80|{big}\n"));
    assert_eq!(
        run_rt(d, &terminal, &["zoom", "80"]).stdout,
        format!("2020-01-02\n80|{big}\n"),
        "a terminal reads a zoom whole"
    );

    let ids = |out: &str| -> Vec<String> {
        out.lines()
            .filter_map(|l| l.split_once('|'))
            .map(|(id, _)| id.to_string())
            .collect()
    };
    let first = run(d, &["grep", "^note", "--time"]).stdout;
    let (newer, footer) = first.trim_end().rsplit_once('\n').unwrap();
    let parts = Regex::new(
        r"^Part 1 of 2, newest first: (\d+) of 80 matches\. Next: ai memory (grep '\^note' --time --part 2 --at [0-9a-f]{40})$",
    )
    .unwrap();
    let caps = parts.captures(footer).expect(footer);
    let shown: usize = caps[1].parse().unwrap();
    assert!(newer.len() < 20_000);
    run(d, &["note", "note lands between two parts"]);
    let args: Vec<&str> = caps[2].split(' ').map(|a| a.trim_matches('\'')).collect();
    let second = run(d, &args).stdout;
    let (older, footer) = second.trim_end().rsplit_once('\n').unwrap();
    assert_eq!(
        footer,
        format!("Part 2 of 2, newest first: {} of 80 matches.", 80 - shown)
    );
    let want: Vec<String> = (0..80).map(|k| k.to_string()).collect();
    assert_eq!(
        [ids(older), ids(newer)].concat(),
        want,
        "every match once, oldest first, from one state"
    );
    let whole = run_rt(d, &terminal, &["grep", "^note"]).stdout;
    assert_eq!(
        ids(&whole),
        [want, vec!["82".into()]].concat(),
        "a terminal reads the latest search whole"
    );
    assert!(whole.ends_with("\n81 matches.\n"), "{whole}");
}

#[test]
fn init_prints_the_block_and_is_idempotent() {
    let (_tmp, d) = store();
    let r = run(&d, &["init"]);
    assert!(r.stdout.starts_with("Found "));
    assert!(
        r.stdout
            .contains("ai memory note \"<1 line, max 280 bytes>\"")
    );
    assert!(r.stdout.contains("ai memory zoom <id+n>"));
    assert!(!r.stdout.contains("build"));
}

#[test]
fn every_store_has_one_origin_stamped_on_what_it_writes() {
    let (_tmp, d) = store();
    let mut s = Store::open(&d).unwrap();
    let origin = s.origin().to_string();
    assert!(
        Regex::new("^[0-9A-HJKMNP-TV-Z]{6}$")
            .unwrap()
            .is_match(&origin)
    );
    let config = fs::read_to_string(d.join("config")).unwrap();
    assert!(config.contains(&format!("origin = {origin}")), "{config}");
    assert_eq!(Store::open(&d).unwrap().origin(), origin);

    let old = Message {
        ts: "20250102T030405Z".into(),
        origin: "OLD123".into(),
        place: Place {
            repo: "acme/widget".into(),
            head: "0123456789ab".into(),
            branch: "main".into(),
            ..Place::default()
        },
        who: Who {
            agent: "codex".into(),
            model: "gpt".into(),
            session: "s9".into(),
        },
        ..msg(Kind::Note, "kept as given")
    };
    s.append("restore", &[old.clone(), msg(Kind::Note, "stamped")])
        .unwrap();
    let snap = s.snapshot().unwrap();
    assert_eq!(snap.message(0).unwrap(), old);
    assert_eq!(snap.message(1).unwrap().origin, origin);
    assert_eq!(snap.node(Coord::leaf(0)).unwrap().unwrap().origin, origin);

    s.set_origin("NEW456").unwrap();
    assert!(s.set_origin("a b").is_err() && s.set_origin("").is_err());
    assert_eq!(Store::open(&d).unwrap().origin(), "NEW456");
    s.append("t", &[msg(Kind::Note, "after")]).unwrap();
    assert_eq!(s.snapshot().unwrap().message(2).unwrap().origin, "NEW456");
}

#[test]
fn the_log_is_in_time_order() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    let at = |ts: &str| Message {
        ts: ts.into(),
        ..msg(Kind::Note, "dated")
    };
    assert!(msg(Kind::Note, "x").ts.is_empty(), "stamped by the store");
    s.append("t", &[msg(Kind::Note, "now")]).unwrap();
    let now = s.snapshot().unwrap().message(0).unwrap().ts;
    assert_eq!(now.len(), 16);
    let Err(e) = s.append("t", &[at("20200101T000000Z")]) else {
        panic!("an earlier ts was appended");
    };
    assert!(
        e.to_string().contains("2020-01-01 00:00 cannot follow"),
        "{e}"
    );
    s.append("t", &[at("29990101T000000Z"), msg(Kind::Note, "late")])
        .unwrap();
    let snap = s.snapshot().unwrap();
    assert_eq!(snap.log_len().unwrap(), 3);
    assert_eq!(
        snap.message(2).unwrap().ts,
        "29990101T000000Z",
        "never stamped before the last message"
    );
}

#[test]
fn the_note_limit_is_a_knob_up_to_one_node() {
    let (_tmp, d) = store();
    assert_eq!(run(&d, &["config", "ENTRY_CHARS=10"]).code, 0);
    assert!(
        run(&d, &["note", "eleven char"])
            .stderr
            .contains("limit 10")
    );
    assert_eq!(run(&d, &["note", "ten chars!"]).code, 0);
    assert!(run(&d, &["init"]).stdout.contains("max 10 bytes"));
    assert!(
        run(&d, &["config", "ENTRY_CHARS=513"])
            .stderr
            .contains("at most 512")
    );
}

#[test]
fn a_bad_knob_names_its_key() {
    let (_tmp, d) = store();
    fs::write(
        d.join("config"),
        fs::read_to_string(d.join("config")).unwrap() + "[ai \"memory\"]\n\tentryChars = lots\n",
    )
    .unwrap();
    let r = run(&d, &["view"]);
    assert!(
        r.stderr
            .contains("ai.memory.entryChars must be a positive whole number"),
        "{}",
        r.stderr
    );
}

#[test]
fn a_missing_dir_is_reported_not_created() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path().join("nope");
    let r = run(&d, &["view"]);
    assert!(r.code == 1 && r.stderr.contains("No memory at"));
    assert!(!d.exists());
}

#[test]
fn a_summariser_logs_timestamped_events_to_summarise_log() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    s.append("t", &[msg(Kind::User, long(0)), msg(Kind::User, long(1))])
        .unwrap();
    let r = run(&d, &["build"]);
    assert_eq!(r.stdout, "Built 3 summaries.\n", "{}", r.stderr);
    let line = |level: &str, event: &str| {
        Regex::new(&format!(
            r"(?m)^\d{{4}}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{{6}}Z  {level} summarise\{{pid={} model=\S+ dir=\S+\}}: ai_tools::memory::\w+: {event}$",
            std::process::id()
        ))
        .unwrap()
    };
    let log = fs::read_to_string(d.join("summarise.log")).unwrap();
    let one = "requests=1 retries=0 tokens=110 cost=0.01 retry_tokens=0 retry_cost=0.0";
    for event in [
        "start".to_string(),
        format!(r"built node=0 bytes=300 secs=[\d.]+ {one}"),
        format!(r"built node=1 bytes=300 secs=[\d.]+ {one}"),
        format!(r"built node=0\+2 bytes=300 secs=[\d.]+ {one}"),
        r"end built=3 secs=[\d.]+ requests=3 retries=0 tokens=330 cost=0.03 retry_tokens=0 retry_cost=0.0".to_string(),
    ] {
        assert!(line("INFO", &event).is_match(&log), "{event}:\n{log}");
    }
    assert_eq!(log.lines().count(), 5, "{log}");

    s.append("t", &[msg(Kind::User, long(2))]).unwrap();
    let failed = AtomicUsize::new(0);
    let flaky = Fake::new(move |_, attempt| {
        if failed.fetch_add(1, Ordering::SeqCst) == 0 {
            bail!("overloaded");
        }
        Ok("y".repeat(if attempt == 1 { 600 } else { 300 }))
    });
    let r = run_rt(&d, &runtime(Arc::new(Model(Arc::new(flaky)))), &["build"]);
    assert!(r.stdout.ends_with("Built 1 summary.\n"), "{}", r.stdout);
    let log = fs::read_to_string(d.join("summarise.log")).unwrap();
    let warns = [
        format!(r#"failed node=2 error="overloaded" secs=[\d.]+ wait=[\d.]+ {one}"#),
        r"over limit node=2 bytes=600 attempt=1 input=100 cache_write=0 cache_read=0 output=10 cost=0.01".to_string(),
    ];
    for event in warns {
        assert!(line("WARN", &event).is_match(&log), "{event}:\n{log}");
    }
    for event in [
        r"built node=2 bytes=300 secs=[\d.]+ requests=2 retries=1 tokens=220 cost=0.02 retry_tokens=110 retry_cost=0.01",
        r"end built=1 secs=[\d.]+ requests=3 retries=1 tokens=330 cost=0.03 retry_tokens=110 retry_cost=0.01",
    ] {
        assert!(line("INFO", event).is_match(&log), "{event}:\n{log}");
    }
}

#[test]
fn a_note_whose_summariser_finds_the_lock_taken_still_gets_built() {
    let (_tmp, d) = store();
    let s = Store::open(&d).unwrap();
    s.append("t", &[msg(Kind::User, long(0))]).unwrap();
    let model: Arc<dyn Backend> = Arc::new(Model(Arc::new(Fake::summaries())));
    let late = Arc::new(Mutex::new(None::<String>));
    let (dir, seen, m2) = (d.clone(), late.clone(), model.clone());
    let mut rt = runtime(model);
    rt.before_release = Box::new(move || {
        let mut seen = seen.lock().unwrap();
        if seen.is_some() {
            return;
        }
        Store::open(&dir)
            .unwrap()
            .append("note", &[msg(Kind::Note, long(1))])
            .unwrap();
        *seen = Some(run_rt(&dir, &runtime(m2.clone()), &["build"]).stdout);
    });
    let r = run_rt(&d, &rt, &["build"]);
    assert_eq!(
        late.lock().unwrap().as_deref(),
        Some("A compactor is already running; it will build these too.\n"),
        "the late summariser must find the lock taken"
    );
    assert_eq!(r.stdout, "Built 3 summaries.\n", "{}", r.stderr);
    let m = Mem::load(&s.snapshot().unwrap(), VIEW).unwrap();
    assert!(m.built(Coord::leaf(1)) && m.built(Coord::new(1, 0)));
    assert_eq!(run(&d, &["build"]).stdout, "Nothing to build.\n");
}

#[test]
fn stats_count_notes_from_the_store_and_reads_from_the_usage_log() {
    let (_tmp, d) = store();
    let r = run(&d, &["stats"]);
    assert!(
        Regex::new(r"^Since \d{4}-\d\d-\d\d, UTC\.\nNo calls\.\n$")
            .unwrap()
            .is_match(&r.stdout),
        "{}{}",
        r.stdout,
        r.stderr
    );
    let s = Store::open(&d).unwrap();
    let note = |ts: &str, agent: &str, repo: &str, session: &str| Message {
        ts: ts.into(),
        place: Place {
            repo: repo.into(),
            ..Place::default()
        },
        who: Who {
            agent: agent.into(),
            model: "m".into(),
            session: session.into(),
        },
        ..msg(Kind::Note, "noted")
    };
    s.append(
        "t",
        &[
            note("20200228T100000Z", "claude-code", "acme/a", "s0"),
            note("20200301T090000Z", "claude-code", "acme/a", "s1"),
            note("20200301T091000Z", "claude-code", "acme/a", "s1"),
            note("20200302T100000Z", "codex", "acme/b", "s2"),
            note("20200302T110000Z", "", "", ""),
        ],
    )
    .unwrap();
    let call = |ts: &str, cmd: &str, ok: bool, agent: &str, repo: &str, session: &str| {
        format!(
            r#"{{"ts":"{ts}","cmd":"{cmd}","args":[],"ok":{ok},"lines":1,"bytes":9,"agent":"{agent}","model":"m","session":"{session}","repo":"{repo}","branch":"main"}}"#
        )
    };
    let (a, b) = (("claude-code", "acme/a"), ("codex", "acme/b"));
    let log = [
        call("20200228T090000Z", "view", true, a.0, a.1, "s0"),
        call("20200301T085900Z", "view", true, a.0, a.1, "s1"),
        call("20200301T092000Z", "zoom", true, a.0, a.1, "s1"),
        call("20200301T092100Z", "zoom", true, a.0, a.1, "s1"),
        call("20200301T092200Z", "grep", false, a.0, a.1, "s1"),
        call("20200301T092300Z", "prompt-hook", true, a.0, a.1, "s1"),
        call("20200301T092400Z", "prompt-hook", true, a.0, a.1, "s1")
            .replace(r#""args":[]"#, r#""args":["reminded","periodic"]"#),
        call("20200301T092500Z", "prompt-hook", false, a.0, a.1, "s1"),
        "torn".into(),
        call("20200302T095900Z", "view", true, b.0, b.1, "s2"),
        call("20200302T100500Z", "show", true, b.0, b.1, "s2"),
        call("20200302T120000Z", "view", true, b.0, b.1, "s3"),
        call("20200302T120100Z", "note", false, b.0, b.1, "s3"),
        call("20200302T120200Z", "note", true, b.0, b.1, "s3"),
        call("20200302T130000Z", "build", true, b.0, b.1, "s3"),
    ];
    fs::write(d.join("usage.jsonl"), log.join("\n") + "\n").unwrap();

    let sessions = "\n\
         3 sessions, 3 woke.\n\
         Per woken session: 1.0 notes, 0.7 zooms, 0.0 greps, 0.3 shows.\n\
         Of these, 67% noted, 33% zoomed, 0% grepped, 33% showed.\n\
         2 prompts hooked, 1 reminded (1 periodic).\n\
         Failed: 1 note, 1 grep.\n";
    let r = run(&d, &["stats", "--since", "2020-03-01"]);
    assert_eq!(
        r.stdout,
        "Since 2020-03-01, UTC.\n\
         day         view  note  zoom  grep  show\n\
         2020-03-01     1     2     2     0     0\n\
         2020-03-02     2     2     0     0     1\n\
         all            3     4     2     0     1\n"
            .to_string()
            + sessions,
        "{}",
        r.stderr
    );
    let r = run(&d, &["stats", "--since", "2020-03-01", "--by", "repo"]);
    assert_eq!(
        r.stdout,
        "Since 2020-03-01, UTC.\n\
         repo    view  note  zoom  grep  show\n\
         acme/a     1     2     2     0     0\n\
         acme/b     2     1     0     0     1\n\
         -          0     1     0     0     0\n\
         all        3     4     2     0     1\n"
            .to_string()
            + sessions
    );
    let r = run(&d, &["stats", "--since", "2020-02-01", "--by", "session"]);
    assert!(
        r.stdout
            .contains("\ns0          1     1     0     0     0\n")
            && r.stdout.contains("\n4 sessions, 4 woke.\n"),
        "{}",
        r.stdout
    );
    let r = run(&d, &["stats", "--since", "2020-03-02", "--by", "agent"]);
    assert!(
        r.stdout.contains(
            "\ncodex     2     1     0     0     1\n-         0     1     0     0     0\n"
        ),
        "{}",
        r.stdout
    );
}

#[test]
fn the_hook_reminds_on_long_prompts_only_and_never_fails() {
    let (_tmp, d) = store();
    let long = "é".repeat(LONG_PROMPT);
    let short = &long[..long.len() - 2];
    let input = |prompt: &str| {
        serde_json::json!({
            "session_id": "s",
            "cwd": "/",
            "hook_event_name": "UserPromptSubmit",
            "prompt": prompt,
        })
        .to_string()
    };
    let r = hook(&d, &input(short));
    assert_eq!((r.code, r.stdout.as_str()), (0, ""), "{}", r.stderr);
    let r = hook(&d, &input(&long));
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        r.stdout,
        "{\"hookSpecificOutput\":{\"additionalContext\":\"Memory: zoom/grep to recall, note to remember.\",\"hookEventName\":\"UserPromptSubmit\"}}\n"
    );
    assert_eq!(REMINDER, "Memory: zoom/grep to recall, note to remember.");
    let r = hook(&d, &serde_json::json!({ "prompt": long }).to_string());
    assert_eq!(r.code, 0);
    assert!(r.stdout.contains(REMINDER), "{}", r.stdout);
    for bad in ["", "not json", r#"{"session_id":"s"}"#, r#"{"prompt":7}"#] {
        let r = hook(&d, bad);
        assert_eq!(
            (r.code, r.stdout.as_str(), r.stderr.as_str()),
            (0, "", ""),
            "{bad}"
        );
    }
    let log: Vec<serde_json::Value> = fs::read_to_string(d.join("usage.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let seen: Vec<_> = log
        .iter()
        .map(|u| {
            (
                u["cmd"].as_str().unwrap(),
                u["args"].clone(),
                u["ok"].as_bool().unwrap(),
            )
        })
        .collect();
    let hook = |args: serde_json::Value, ok| ("prompt-hook", args, ok);
    assert_eq!(
        seen,
        [
            hook(serde_json::json!([]), true),
            hook(serde_json::json!(["reminded", "long"]), true),
            hook(serde_json::json!(["reminded", "long"]), true),
            hook(serde_json::json!([]), false),
            hook(serde_json::json!([]), false),
            hook(serde_json::json!([]), false),
            hook(serde_json::json!([]), false),
        ]
    );
    assert_eq!(
        log[3]["error"],
        "Not a hook input with a prompt.: EOF while parsing a value at line 1 column 0"
    );
    assert_eq!(log[1]["lines"], 1);
    let r = run(&d, &["stats"]);
    assert!(r.stdout.ends_with("No calls.\n"), "{}", r.stdout);
}

#[test]
fn the_hook_reminds_every_few_short_prompts_per_session() {
    let (_tmp, d) = store();
    let say = |d: &Path, session: &str, prompt: &str| {
        let r = hook(
            d,
            &serde_json::json!({ "session_id": session, "prompt": prompt }).to_string(),
        );
        assert_eq!(r.code, 0, "{}", r.stderr);
        r.stdout.contains(REMINDER)
    };
    let long = "x".repeat(LONG_PROMPT);
    let shorts = |d: &Path, session: &str, n: usize| -> Vec<bool> {
        (0..n).map(|_| say(d, session, "hi")).collect()
    };
    let every = REMIND_EVERY;
    let mut due = vec![false; every - 1];
    due.push(true);
    assert_eq!(shorts(&d, "a", every), due);
    assert_eq!(shorts(&d, "a", every), due);
    assert!(say(&d, "a", &long));
    assert_eq!(shorts(&d, "b", 2), [false, false]);
    assert_eq!(shorts(&d, "a", every), due);
    assert_eq!(shorts(&d, "b", every - 2), due[2..]);
    let args: Vec<serde_json::Value> = fs::read_to_string(d.join("usage.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["args"].clone())
        .filter(|a| a.as_array().is_some_and(|a| !a.is_empty()))
        .collect();
    let (p, l) = (
        serde_json::json!(["reminded", "periodic"]),
        serde_json::json!(["reminded", "long"]),
    );
    assert_eq!(args, [p.clone(), p.clone(), l, p.clone(), p]);

    let (_tmp, d) = store();
    fs::create_dir(d.join("usage.jsonl")).unwrap();
    assert_eq!(shorts(&d, "a", 2 * every), vec![false; 2 * every]);
    assert!(say(&d, "a", &long));
}
