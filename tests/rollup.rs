use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use ai_tools::{
    chat::{
        Built, Chat, Id, Key, Line, LineKind, OnFail, Rollup, Source, Thread, build, dry_run,
        rebuild, rewrite, rollup, summarise,
    },
    memory::{Backend, Block, Conversation, JOBS, NODE, SCALE, Usage, claude},
};
use anyhow::{Result, bail};
use indicatif::ProgressBar;
use regex::Regex;
use serde_json::{Value, json};
use tempfile::TempDir;

/// A Claude Code transcript, entry by entry; each entry is one item, numbered in file order.
struct Script {
    lines: Vec<String>,
    last: Option<String>,
}

impl Script {
    fn new() -> Script {
        Script {
            lines: Vec::new(),
            last: None,
        }
    }

    /// Adds `v` after the last entry; returns its item's sequence number.
    fn entry(&mut self, mut v: Value) -> u64 {
        let n = self.lines.len() as u64;
        let uuid = format!("e{n}");
        v["uuid"] = uuid.clone().into();
        v["parentUuid"] = self.last.clone().into();
        v["timestamp"] = format!("2026-10-15T10:{:02}:{:02}.000Z", n / 60, n % 60).into();
        v["sessionId"] = "s1".into();
        self.lines.push(format!("{v}\n"));
        self.last = Some(uuid);
        n
    }

    fn user(&mut self, text: &str) -> u64 {
        self.entry(json!({"type": "user", "origin": {"kind": "human"},
            "message": {"role": "user", "content": text}}))
    }

    fn ai(&mut self, text: &str) -> u64 {
        self.entry(json!({"type": "assistant",
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}))
    }

    fn call(&mut self, name: &str, input: Value) -> u64 {
        let id = format!("toolu_{}", self.lines.len());
        self.entry(json!({"type": "assistant", "message": {"role": "assistant",
            "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}}))
    }

    /// The result of the call numbered `call`.
    fn result(&mut self, call: u64, output: &str) -> u64 {
        self.entry(json!({"type": "user", "message": {"role": "user",
            "content": [{"type": "tool_result", "tool_use_id": format!("toolu_{call}"),
                "content": output}]}}))
    }

    /// The next entry hangs on entry `seq`: a rewind.
    fn at(&mut self, seq: u64) {
        self.last = Some(format!("e{seq}"));
    }

    /// A turn of a short prompt and a reply too long to be its own line.
    fn turn(&mut self, k: usize) -> u64 {
        self.user(&format!("task {k}"));
        self.ai(&format!("done {k}: {}", "r".repeat(600)))
    }

    /// `n` steps of a text, a Read and a long output; returns the last.
    fn steps(&mut self, n: usize) -> u64 {
        let mut last = 0;
        for j in 0..n {
            self.ai(&format!("step {j}"));
            let c = self.call("Read", json!({"file_path": format!("/src/f{j}.rs")}));
            last = self.result(c, &"x".repeat(10_000));
        }
        last
    }
}

struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Repo {
        let dir = TempDir::new().unwrap();
        gix::init(dir.path().join("repo")).unwrap();
        let r = Repo { dir };
        r.git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "root",
        ]);
        r
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    /// Where the chat stores live.
    fn base(&self) -> PathBuf {
        self.dir.path().join("chat")
    }

    fn chat(&self) -> Chat {
        Chat::find_in(&self.base(), &self.path()).unwrap().unwrap()
    }

    /// The thread `t`, with `s` attached as session `s1`.
    fn thread(&self, s: &Script) -> Thread {
        let p = self.dir.path().join("s1.jsonl");
        fs::write(&p, s.lines.concat()).unwrap();
        let chat = self.chat();
        chat.attach("t", "claude", "s1", Some(&p)).unwrap();
        chat.thread("t").unwrap()
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A model that answers each call with a 300-byte line naming its verb and the first and last
/// id in its input (`t.i|` items, `[t.i]` in lines), so lines show what they cover and two
/// never merge free.
struct Fake {
    delay: Duration,
    fail: Box<dyn Fn(&str) -> bool + Send + Sync>,
    tasks: Mutex<Vec<String>>,
    stopped: AtomicBool,
}

impl Fake {
    fn new() -> Arc<Fake> {
        Fake::with(Duration::from_millis(2), |_| false)
    }

    fn with(delay: Duration, fail: impl Fn(&str) -> bool + Send + Sync + 'static) -> Arc<Fake> {
        Arc::new(Fake {
            delay,
            fail: Box::new(fail),
            tasks: Mutex::default(),
            stopped: AtomicBool::new(false),
        })
    }

    fn backend(self: &Arc<Fake>) -> Arc<dyn Backend> {
        Arc::new(Model(self.clone()))
    }

    fn tasks(&self) -> Vec<String> {
        self.tasks.lock().unwrap().clone()
    }

    fn calls(&self) -> usize {
        self.tasks.lock().unwrap().len()
    }
}

fn reply(task: &str) -> String {
    let input = task.split_once("<input>\n").expect("an input").1;
    let verb = [
        ("Compress this turn", "turn"),
        ("Compress this part", "part"),
        ("Compress this long turn", "chunked"),
        ("Merge these", "merge"),
    ]
    .into_iter()
    .find(|v| task.contains(v.0))
    .expect("a known task")
    .1;
    let ids: Vec<&str> = Regex::new(r"(?m)^(\d+(?:\.\d+)?(?:\+\d+)?)\||\[(\d+\.\d+)\]")
        .unwrap()
        .captures_iter(input)
        .map(|c| c.get(1).or(c.get(2)).unwrap().as_str())
        .collect();
    let line = format!("{verb} [{}]..[{}]", ids[0], ids[ids.len() - 1]);
    format!("{line:.<300}")
}

struct Model(Arc<Fake>);

impl Backend for Model {
    fn agent(&self) -> &str {
        "fake-harness"
    }

    fn model(&self) -> &str {
        "fake"
    }

    fn open(&self, system: &str) -> Result<Box<dyn Conversation>> {
        assert_eq!(system, rollup::PROMPT);
        Ok(Box::new(Conv(self.0.clone())))
    }

    fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
    }
}

struct Conv(Arc<Fake>);

impl Conversation for Conv {
    fn say(&mut self, message: &[Block]) -> Result<String> {
        let [b] = message else {
            panic!("one block: {message:?}")
        };
        assert!(b.text.contains(SCALE), "the ruler");
        self.0.tasks.lock().unwrap().push(b.text.clone());
        thread::sleep(self.0.delay);
        if (self.0.fail)(&b.text) {
            bail!("fake failure");
        }
        Ok(reply(&b.text))
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

fn lines(t: &Thread) -> BTreeMap<Key, Line> {
    t.snapshot().unwrap().all_lines().unwrap()
}

fn texts(t: &Thread) -> BTreeMap<String, String> {
    lines(t)
        .into_iter()
        .map(|(k, l)| (k.to_string(), l.text))
        .collect()
}

fn key(level: u32, last: u64) -> Key {
    Key { level, last }
}

fn summarised(t: &Thread, fake: &Arc<Fake>) -> Built {
    summarise(t, fake.backend(), OnFail::Continue, &ProgressBar::hidden())
        .unwrap()
        .expect("the lock is free")
}

fn state(r: &Repo) -> String {
    let chat = r.chat();
    chat.thread("t")
        .unwrap()
        .snapshot()
        .unwrap()
        .mark()
        .to_string()
}

#[test]
fn short_turns_and_their_nodes_are_free_lines() {
    let r = Repo::new();
    let mut s = Script::new();
    s.user("hi");
    let a = s.ai("Hello.");
    s.user("thanks");
    let b = s.ai("Welcome.");
    s.user("bye");
    let t = r.thread(&s);
    let fake = Fake::new();
    let built = summarised(&t, &fake);
    assert_eq!(fake.calls(), 0);
    assert_eq!((built.lines, built.calls), (3, 0));
    let l = lines(&t);
    assert_eq!(
        l.keys().copied().collect::<Vec<_>>(),
        [key(1, a), key(1, b), key(2, b)]
    );
    assert_eq!(l[&key(1, a)].text, "user: hi ai: Hello.");
    assert_eq!(l[&key(1, a)].kind, LineKind::Turn);
    assert_eq!(l[&key(1, a)].who.model, "");
    assert_eq!(
        l[&key(2, b)].text,
        "user: hi ai: Hello. user: thanks ai: Welcome."
    );
    assert_eq!(l[&key(2, b)].kind, LineKind::Node);
}

#[test]
fn every_level_is_built_and_only_once() {
    let r = Repo::new();
    let mut s = Script::new();
    let e0 = s.turn(0);
    let e1 = s.turn(1);
    let big = s.user("read every file");
    s.steps(14);
    let e2 = s.ai("read them all");
    let e3 = s.turn(3);
    let e4 = s.turn(4);
    s.user("next");
    let t = r.thread(&s);

    let tree = t.snapshot().unwrap().tree().unwrap();
    let roll = Rollup::new(&tree);
    let b = roll.branch(&tree, *tree.tips().last().unwrap());
    assert_eq!(b.parts.len(), 6);
    let chunks = b.chunk_keys(&b.parts[2]);
    assert_eq!(chunks.len(), 2, "14 steps of about 4.2 KB in 48 KB chunks");
    assert_eq!(chunks[1], key(0, e2));
    assert_eq!(
        b.lines(),
        [key(1, e0), key(1, e1), key(1, e2), key(1, e3), key(1, e4)]
    );
    assert!(b.line(&b.parts[5]).is_none(), "the open turn has no line");
    assert_eq!(
        roll.source(key(1, e2)),
        Some(&Source::Chunked {
            message: Some(big),
            chunks: chunks.clone()
        })
    );

    let id = |seq: u64| Id::item(tree.place(seq as usize)).to_string();
    assert_eq!(
        (id(e1), id(big), id(e2), id(e4)),
        ("1.1".into(), "2.0".into(), "2.43".into(), "4.1".into())
    );
    let fake = Fake::new();
    let built = summarised(&t, &fake);
    let span = |kind: &str, a: u64, b: u64| format!("{kind} [{}]..[{}]", id(a), id(b));
    let want = [
        (chunks[0].to_string(), span("part", big + 1, chunks[0].last)),
        (format!("0@{e2}"), span("part", chunks[0].last + 1, e2)),
        (format!("1@{e0}"), span("turn", 0, e0)),
        (format!("1@{e1}"), span("turn", e0 + 1, e1)),
        (format!("1@{e2}"), span("chunked", big, e2)),
        (format!("1@{e3}"), span("turn", e2 + 1, e3)),
        (format!("1@{e4}"), span("turn", e3 + 1, e4)),
        (format!("2@{e1}"), span("merge", 0, e1)),
        (format!("2@{e3}"), span("merge", big, e3)),
        (format!("3@{e3}"), span("merge", 0, e3)),
    ];
    let got = texts(&t);
    assert_eq!(got.len(), want.len(), "{got:#?}");
    for (k, prefix) in &want {
        assert!(got[k].starts_with(&format!("{prefix}.")), "{k}: {}", got[k]);
    }
    assert_eq!((built.lines, built.calls), (10, 10));
    assert_eq!(fake.calls(), 10, "each line once");
    let s = built.spent;
    assert_eq!((s.requests, s.retries, s.all.tokens()), (10, 0, 1100));
    assert!((s.all.cost - 0.1).abs() < 1e-9, "{s:?}");
    assert_eq!(s.retried, Usage::default());
    let l = lines(&t);
    assert_eq!(l[&chunks[0]].kind, LineKind::Chunk);
    assert_eq!(l[&key(1, e2)].kind, LineKind::Turn);
    assert_eq!(l[&key(3, e3)].kind, LineKind::Node);
    assert_eq!(
        (
            l[&key(1, e0)].who.agent.as_str(),
            l[&key(1, e0)].who.model.as_str()
        ),
        ("fake-harness", "fake")
    );
    assert!(l.values().all(|l| l.text.len() <= NODE));

    let tasks = fake.tasks();
    let chunk_tasks: Vec<&String> = tasks
        .iter()
        .filter(|t| t.contains("part of a long"))
        .collect();
    let context = "<context>\n2.0|user: read every file\n</context>";
    assert!(chunk_tasks.iter().all(|t| t.contains(context)));
    assert!(
        chunk_tasks
            .iter()
            .any(|t| t.contains("2.2|tool Read: file_path=/src/f0.rs"))
    );
    let whole = tasks
        .iter()
        .find(|t| t.contains("long turn, given"))
        .unwrap();
    let n = chunks[0].last - big;
    assert!(
        whole.contains(&format!(
            "<input>\n2.0|user: read every file\n2.1+{n}|part [2.1]..[2.{n}]"
        )),
        "{whole}"
    );
    assert!(tasks.iter().all(|t| t.ends_with("</input>")));
}

#[test]
fn a_second_summariser_finds_nothing_to_build() {
    let r = Repo::new();
    let mut s = Script::new();
    for k in 0..5 {
        s.turn(k);
    }
    s.user("open");
    let t = r.thread(&s);
    let fake = Fake::new();
    assert_eq!(summarised(&t, &fake).calls, 5 + 2 + 1);
    let (before, at) = (texts(&t), state(&r));
    let again = Fake::new();
    assert_eq!(summarised(&t, &again), Built::default());
    assert_eq!(again.calls(), 0);
    assert_eq!((texts(&t), state(&r)), (before, at), "no write");
    let keys: Vec<Key> = lines(&t).into_keys().collect();
    let b = build(
        &t,
        again.backend(),
        &keys,
        Instant::now() + Duration::from_secs(5),
        &ProgressBar::hidden(),
    )
    .unwrap();
    assert_eq!((b, again.calls()), (Built::default(), 0));
}

#[test]
fn branches_share_the_lines_of_their_common_prefix() {
    let r = Repo::new();
    let mut s = Script::new();
    let e0 = s.turn(0);
    let e1 = s.turn(1);
    let a2 = s.turn(2);
    let a3 = s.turn(3);
    s.user("open a");
    s.at(e1);
    let b2 = s.turn(12);
    let b3 = s.turn(13);
    s.user("open b");
    let t = r.thread(&s);
    let tree = t.snapshot().unwrap().tree().unwrap();
    let roll = Rollup::new(&tree);
    let tips = tree.tips();
    assert_eq!(tips.len(), 2);
    let a = roll.branch(&tree, tips[0]).lines();
    let b = roll.branch(&tree, tips[1]).lines();
    assert_eq!(a, [key(1, e0), key(1, e1), key(1, a2), key(1, a3)]);
    assert_eq!(b, [key(1, e0), key(1, e1), key(1, b2), key(1, b3)]);

    let fake = Fake::new();
    summarised(&t, &fake);
    let got: Vec<Key> = lines(&t).into_keys().collect();
    let mut want = vec![
        key(1, e0),
        key(1, e1),
        key(2, e1),
        key(1, a2),
        key(1, a3),
        key(2, a3),
        key(3, a3),
        key(1, b2),
        key(1, b3),
        key(2, b3),
        key(3, b3),
    ];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(fake.calls(), want.len(), "the shared prefix once");
    let l = texts(&t);
    assert!(l[&format!("3@{b3}")].starts_with("merge [0.0]..[3.1]."));
}

#[test]
fn an_open_turn_gets_its_finished_chunks() {
    let r = Repo::new();
    let mut s = Script::new();
    s.user("survey the code");
    let last = s.steps(30);
    let t = r.thread(&s);
    let tree = t.snapshot().unwrap().tree().unwrap();
    let roll = Rollup::new(&tree);
    let b = roll.branch(&tree, *tree.tips().last().unwrap());
    let keys = b.chunk_keys(&b.parts[0]);
    assert_eq!(keys.len(), 2, "two full chunks; the third may grow");
    assert!(keys[1].last < last);
    let fake = Fake::new();
    assert_eq!(summarised(&t, &fake).calls, 2);
    assert_eq!(lines(&t).into_keys().collect::<Vec<_>>(), keys);
}

#[test]
fn build_makes_the_lines_asked_and_what_they_need() {
    let r = Repo::new();
    let mut s = Script::new();
    let e0 = s.turn(0);
    let e1 = s.turn(1);
    s.user("read every file");
    let e2 = s.steps(14);
    s.turn(3);
    s.user("open");
    let t = r.thread(&s);
    let fake = Fake::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let b = build(
        &t,
        fake.backend(),
        &[key(2, e1), key(1, e2)],
        deadline,
        &ProgressBar::hidden(),
    )
    .unwrap();
    assert_eq!((b.lines, b.calls), (6, 6));
    let got: Vec<String> = texts(&t).into_keys().collect();
    assert_eq!(got.len(), 6, "{got:?}");
    for k in [key(1, e0), key(1, e1), key(2, e1), key(1, e2)] {
        assert!(got.contains(&k.to_string()), "{k}");
    }
    let err = build(
        &t,
        fake.backend(),
        &[key(1, e0 + 1)],
        deadline,
        &ProgressBar::hidden(),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("1@{} is not a line of thread t.", e0 + 1)
    );
}

#[test]
fn build_returns_at_its_deadline() {
    let r = Repo::new();
    let mut s = Script::new();
    let e0 = s.turn(0);
    s.user("hi");
    let e1 = s.ai("hello");
    s.user("open");
    let t = r.thread(&s);
    let slow = Fake::with(Duration::from_secs(3), |_| false);
    let at = Instant::now();
    let b = build(
        &t,
        slow.backend(),
        &[key(1, e0), key(1, e1)],
        at + Duration::from_millis(200),
        &ProgressBar::hidden(),
    )
    .unwrap();
    assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
    assert_eq!((b.lines, b.calls), (1, 0), "the free line only");
    assert_eq!(slow.calls(), 1);
    assert!(
        slow.stopped.load(Ordering::SeqCst),
        "the running call is stopped"
    );
    assert_eq!(lines(&t).into_keys().collect::<Vec<_>>(), [key(1, e1)]);
}

#[test]
fn a_held_lock_makes_a_second_summariser_exit_at_once() {
    let r = Repo::new();
    let mut s = Script::new();
    s.turn(0);
    s.user("open");
    let t = r.thread(&s);
    let fake = Fake::new();
    let lock = t.try_lock().unwrap().expect("free");
    assert_eq!(
        summarise(&t, fake.backend(), OnFail::Continue, &ProgressBar::hidden()).unwrap(),
        None
    );
    assert_eq!(fake.calls(), 0);
    let other = r.chat().thread("t").unwrap();
    assert!(other.try_lock().unwrap().is_none());
    drop(lock);
    assert_eq!(summarised(&t, &fake).calls, 1);
    let log = fs::read_to_string(t.log_path().unwrap()).unwrap();
    assert!(
        log.contains("busy") && log.contains("built") && log.contains(" end"),
        "{log}"
    );
}

#[test]
fn build_waits_for_a_running_summariser_and_calls_for_nothing_it_built() {
    let r = Repo::new();
    let mut s = Script::new();
    for k in 0..3 {
        s.turn(k);
    }
    s.user("open");
    let t = r.thread(&s);
    let tree = t.snapshot().unwrap().tree().unwrap();
    let keys = Rollup::new(&tree)
        .branch(&tree, *tree.tips().last().unwrap())
        .lines();
    let slow = Fake::with(Duration::from_millis(300), |_| false);
    let summarising = {
        let (base, path, slow) = (r.base(), r.path(), slow.clone());
        thread::spawn(move || {
            let t = Chat::find_in(&base, &path)
                .unwrap()
                .unwrap()
                .thread("t")
                .unwrap();
            summarise(&t, slow.backend(), OnFail::Continue, &ProgressBar::hidden()).unwrap()
        })
    };
    while slow.calls() == 0 {
        thread::sleep(Duration::from_millis(5));
    }
    let fast = Fake::new();
    let at = Instant::now();
    let b = build(
        &t,
        fast.backend(),
        &keys,
        at + Duration::from_secs(10),
        &ProgressBar::hidden(),
    )
    .unwrap();
    assert!(
        at.elapsed() >= Duration::from_millis(200),
        "{:?}",
        at.elapsed()
    );
    assert_eq!((b, fast.calls()), (Built::default(), 0));
    let summarised = summarising
        .join()
        .unwrap()
        .expect("the summariser held the lock");
    assert_eq!(summarised.calls as usize, slow.calls(), "one call per line");
    let built = lines(&t);
    assert!(keys.iter().all(|k| built.contains_key(k)), "{built:?}");
}

#[test]
fn build_gives_up_on_a_lock_held_past_its_deadline() {
    let r = Repo::new();
    let mut s = Script::new();
    let e0 = s.turn(0);
    s.user("open");
    let t = r.thread(&s);
    let held = r
        .chat()
        .thread("t")
        .unwrap()
        .try_lock()
        .unwrap()
        .expect("free");
    let fake = Fake::new();
    let at = Instant::now();
    let b = build(
        &t,
        fake.backend(),
        &[key(1, e0)],
        at + Duration::from_millis(200),
        &ProgressBar::hidden(),
    )
    .unwrap();
    let took = at.elapsed();
    assert!(
        took >= Duration::from_millis(200) && took < Duration::from_secs(2),
        "{took:?}"
    );
    assert_eq!((b, fake.calls()), (Built::default(), 0));
    assert!(lines(&t).is_empty());
    let log = fs::read_to_string(t.log_path().unwrap()).unwrap();
    assert!(log.contains("waiting") && log.contains("deadline"), "{log}");
    drop(held);
}

#[test]
fn a_dry_run_builds_in_memory_and_stores_nothing() {
    let r = Repo::new();
    let mut s = Script::new();
    let e0 = s.turn(0);
    let e1 = s.turn(1);
    s.user("open");
    let t = r.thread(&s);
    let old = Fake::new();
    assert_eq!(summarised(&t, &old).calls, 3);
    let (before, at) = (texts(&t), state(&r));

    let fake = Fake::new();
    let d = dry_run(&t, fake.backend(), false, None).unwrap();
    assert_eq!(
        (d.built, d.lines.len(), fake.calls()),
        (Built::default(), 0, 0)
    );

    let d = dry_run(&t, fake.backend(), true, None).unwrap();
    assert_eq!((d.built.lines, d.built.calls, fake.calls()), (3, 3, 3));
    let got: Vec<(u64, Key, bool)> = d
        .lines
        .iter()
        .map(|l| {
            (
                l.first,
                l.key,
                l.was.as_deref() == Some(&before[&l.key.to_string()]),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (0, key(1, e0), true),
            (2, key(1, e1), true),
            (0, key(2, e1), true)
        ]
    );
    assert!(
        d.lines
            .iter()
            .all(|l| l.text.starts_with("turn ") || l.text.starts_with("merge "))
    );
    assert_eq!((texts(&t), state(&r)), (before, at), "nothing stored");
}

#[test]
fn rewrite_drops_the_line_and_those_above_it() {
    let r = Repo::new();
    let mut s = Script::new();
    let e0 = s.turn(0);
    let e1 = s.turn(1);
    s.turn(2);
    let e3 = s.turn(3);
    s.user("open");
    let t = r.thread(&s);
    assert_eq!(summarised(&t, &Fake::new()).calls, 4 + 2 + 1);
    let before = texts(&t);

    let fake = Fake::with(Duration::from_millis(2), |_| false);
    let d = dry_run(&t, fake.backend(), true, Some(key(2, e1))).unwrap();
    assert_eq!(
        d.lines
            .iter()
            .map(|l| (l.key, l.was.clone()))
            .collect::<Vec<_>>(),
        [(key(2, e1), Some(before[&format!("2@{e1}")].clone()))],
        "a dry rewrite writes that line alone"
    );
    let d = dry_run(&t, fake.backend(), false, Some(key(1, e0))).unwrap();
    assert!(d.lines.is_empty(), "a written line has nothing to write");
    assert_eq!(texts(&t), before, "nothing stored");

    let w = rewrite(
        &t,
        fake.backend(),
        key(1, e0),
        Instant::now() + Duration::from_secs(5),
        &ProgressBar::hidden(),
    )
    .unwrap()
    .expect("the lock is free");
    assert_eq!(
        (w.dropped, w.above, w.built.lines, w.built.calls),
        (3, 2, 1, 1)
    );
    let got: Vec<Key> = lines(&t).into_keys().collect();
    assert!(got.contains(&key(1, e0)) && !got.contains(&key(2, e1)) && !got.contains(&key(3, e3)));
    assert_eq!(got.len(), 5);
    assert_eq!(summarised(&t, &fake).calls, 2, "the lines above, again");
    assert_eq!(lines(&t).len(), 7);

    let err = rewrite(
        &t,
        fake.backend(),
        key(1, e0 + 1),
        Instant::now(),
        &ProgressBar::hidden(),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("1@{} is not a line of thread t.", e0 + 1)
    );
}

#[test]
fn a_dry_run_names_its_lines_by_id() {
    let r = Repo::new();
    let mut s = Script::new();
    s.turn(0);
    s.turn(1);
    let big = s.user("read every file");
    s.steps(14);
    s.ai("read them all");
    s.turn(3);
    s.user("next");
    let t = r.thread(&s);
    let tree = t.snapshot().unwrap().tree().unwrap();
    let d = dry_run(&t, Fake::new().backend(), false, None).unwrap();
    let place = |seq: u64| tree.place(seq as usize);
    let ids: Vec<String> = d
        .lines
        .iter()
        .map(|l| Id::line(l.key.level, place(l.first), place(l.key.last)).to_string())
        .collect();
    let chunks: Vec<&String> = ids.iter().filter(|i| i.starts_with("2.")).collect();
    assert_eq!(chunks.len(), 2, "{ids:?}");
    let n = chunks[0]
        .strip_prefix("2.1+")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(*chunks[1], format!("2.{}+{}", n + 1, 43 - n));
    let mut rest: Vec<&String> = ids.iter().filter(|i| !i.starts_with("2.")).collect();
    rest.sort();
    assert_eq!(rest, ["0", "0+2", "0+4", "1", "2", "2+2", "3"]);
    assert_eq!(place(big).turn, 2);
}

#[test]
fn rebuild_drops_every_line_and_writes_them_again() {
    let r = Repo::new();
    let mut s = Script::new();
    for k in 0..3 {
        s.turn(k);
    }
    s.user("open");
    let t = r.thread(&s);
    let fake = Fake::new();
    let first = summarised(&t, &fake).calls;
    let before = texts(&t);
    let again = Fake::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (dropped, b) = rebuild(&t, again.backend(), deadline, &ProgressBar::hidden())
        .unwrap()
        .expect("the lock is free");
    assert_eq!(dropped, before.len());
    assert_eq!((b.calls, again.calls() as u64), (first, first));
    assert_eq!(
        texts(&t).into_keys().collect::<Vec<_>>(),
        before.into_keys().collect::<Vec<_>>()
    );

    let held = t.try_lock().unwrap().expect("free");
    let at = Instant::now();
    assert_eq!(
        rebuild(
            &t,
            again.backend(),
            at + Duration::from_millis(200),
            &ProgressBar::hidden()
        )
        .unwrap(),
        None
    );
    assert!(at.elapsed() >= Duration::from_millis(200));
    assert_eq!(
        texts(&t).len(),
        dropped,
        "nothing dropped while the lock is held"
    );
    drop(held);
}

#[test]
fn a_run_that_stops_at_a_failure_starts_no_call_after_it() {
    let r = Repo::new();
    let mut s = Script::new();
    s.user("poison");
    s.ai(&"p".repeat(600));
    for k in 1..2 * JOBS {
        s.turn(k);
    }
    s.user("open");
    let t = r.thread(&s);
    let fake = Fake::with(Duration::ZERO, |t| {
        let poison = t.contains("poison");
        if !poison {
            thread::sleep(Duration::from_millis(300));
        }
        poison
    });
    let b = summarise(&t, fake.backend(), OnFail::Stop, &ProgressBar::hidden())
        .unwrap()
        .expect("the lock is free");
    assert_eq!(b.failed.len(), 1);
    assert_eq!(fake.calls(), JOBS, "only the calls running at the failure");
    assert_eq!(b.calls as usize, JOBS - 1, "their lines are kept");
    assert_eq!(lines(&t).len(), JOBS - 1);
}

#[test]
fn failed_lines_stay_unbuilt_and_a_failing_model_stops_the_summariser() {
    let r = Repo::new();
    let mut s = Script::new();
    let e0 = s.turn(0);
    s.user("poison");
    let e1 = s.ai(&"p".repeat(600));
    let e2 = s.turn(2);
    s.user("open");
    let t = r.thread(&s);
    let fake = Fake::with(Duration::from_millis(2), |t| t.contains("poison"));
    let b = summarised(&t, &fake);
    assert_eq!(b.failed.len(), 1);
    assert_eq!(b.failed[0].0, key(1, e1));
    assert!(b.failed[0].1.contains("fake failure"));
    assert_eq!(
        lines(&t).into_keys().collect::<Vec<_>>(),
        [key(1, e0), key(1, e2)],
        "the node over the failed line waits"
    );

    let r = Repo::new();
    let mut s = Script::new();
    for k in 0..5 * JOBS {
        s.turn(k);
    }
    s.user("open");
    let t = r.thread(&s);
    let dead = Fake::with(Duration::from_millis(2), |_| true);
    let b = summarised(&t, &dead);
    assert_eq!((b.lines, b.failed.len()), (0, dead.calls()));
    assert!(
        (JOBS..2 * JOBS).contains(&dead.calls()),
        "a wave of failures stops it: {} calls",
        dead.calls()
    );
}

#[test]
fn config_sets_the_model_and_switches_the_summariser_off() {
    let r = Repo::new();
    let chat = r.chat();
    let c = chat.config().unwrap();
    assert_eq!((c.model, c.summarise), (claude::model(), true));
    r.git(&["config", "ai.chat.model", "haiku"]);
    r.git(&["config", "ai.chat.summarise", "false"]);
    let chat = r.chat();
    let c = chat.config().unwrap();
    assert_eq!((c.model.as_str(), c.summarise), ("haiku", false));
    let mut s = Script::new();
    s.user("hi");
    let t = r.thread(&s);
    assert!(
        !chat.spawn_summariser(&t).unwrap(),
        "switched off: nothing starts"
    );
    r.git(&["config", "ai.chat.summarise", "maybe"]);
    let err = r.chat().config().unwrap_err();
    assert!(
        format!("{err:#}").contains("ai.chat.summarise is true or false"),
        "{err:#}"
    );
}
