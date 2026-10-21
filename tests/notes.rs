use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    sync::{Arc, Mutex},
};

use ai_tools::memory::{Backend, Block, Conversation, PLACEHOLDER};
use ai_tools::notes::{Cli, LEAF_MAX, PROMPT, Runtime, SCOPE_MAX, SUMMARY};
use anyhow::Result;
use clap::Parser;
use regex::Regex;
use tempfile::TempDir;

struct Out {
    code: u8,
    stdout: String,
    stderr: String,
}

/// A model that answers `scope of <topic>` and a gist counting the items it was given; `long`
/// first answers with an overlong scope.
#[derive(Default)]
struct Fake {
    long: bool,
    inputs: Mutex<Vec<String>>,
    retries: Mutex<Vec<String>>,
}

impl Fake {
    fn topics(&self) -> Vec<String> {
        let re = Regex::new(r"\nTopic: (\S+)\n").unwrap();
        self.inputs
            .lock()
            .unwrap()
            .iter()
            .map(|i| re.captures(i).unwrap()[1].to_string())
            .collect()
    }

    fn take(&self) -> Vec<String> {
        let t = self.topics();
        self.inputs.lock().unwrap().clear();
        t
    }
}

struct Model(Arc<Fake>);

struct Conv {
    fake: Arc<Fake>,
    input: String,
}

impl Backend for Model {
    fn agent(&self) -> &str {
        "fake-harness"
    }

    fn model(&self) -> &str {
        "fake"
    }

    fn open(&self, system: &str) -> Result<Box<dyn Conversation>> {
        assert_eq!(system, PROMPT);
        Ok(Box::new(Conv {
            fake: self.0.clone(),
            input: String::new(),
        }))
    }
}

impl Conversation for Conv {
    fn say(&mut self, message: &[Block]) -> Result<String> {
        let [m] = message else {
            panic!("one block: {message:?}")
        };
        let first = self.input.is_empty();
        if first {
            self.input = m.text.clone();
            self.fake.inputs.lock().unwrap().push(m.text.clone());
        } else {
            self.fake.retries.lock().unwrap().push(m.text.clone());
        }
        let topic = Regex::new(r"\nTopic: (\S+)\n")
            .unwrap()
            .captures(&self.input)
            .unwrap()[1]
            .to_string();
        let items = self.input.matches("\n- ").count();
        let scope = match self.fake.long && first {
            true => "x".repeat(SCOPE_MAX + 1),
            false => format!("scope of {topic}"),
        };
        Ok(format!("{scope}\n\ngist of {topic}: {items} items"))
    }

    fn session(&self) -> String {
        "fake-session".into()
    }
}

fn runtime(fake: &Arc<Fake>, stdin: &str) -> Runtime {
    let backend: Arc<dyn Backend> = Arc::new(Model(fake.clone()));
    let stdin = stdin.to_string();
    Runtime {
        backend: Box::new(move |_| Ok(backend.clone())),
        summarise_on_change: false,
        stdin: Box::new(move || Ok(stdin.clone())),
    }
}

fn run_rt(dir: &Path, rt: &Runtime, args: &[&str]) -> Out {
    let cli = match Cli::try_parse_from(std::iter::once("notes").chain(args.iter().copied())) {
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

struct Notes {
    _tmp: TempDir,
    dir: PathBuf,
    fake: Arc<Fake>,
}

impl Notes {
    fn new() -> Notes {
        Notes::with(Fake::default())
    }

    fn with(fake: Fake) -> Notes {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("notes");
        Notes {
            _tmp: tmp,
            dir,
            fake: Arc::new(fake),
        }
    }

    fn run_in(&self, stdin: &str, args: &[&str]) -> Out {
        run_rt(&self.dir, &runtime(&self.fake, stdin), args)
    }

    /// Runs `args`, which must succeed; its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let r = self.run_in("", args);
        assert_eq!(r.code, 0, "{args:?}: {}", r.stderr);
        r.stdout
    }

    fn fail(&self, args: &[&str]) -> String {
        let r = self.run_in("", args);
        assert_ne!(r.code, 0, "{args:?} succeeded: {}", r.stdout);
        r.stderr
    }

    /// Files a note titled by its text; its id.
    fn add(&self, topic: &str, text: &str) -> String {
        self.add_titled(topic, text, text)
    }

    fn add_titled(&self, topic: &str, title: &str, text: &str) -> String {
        let out = self.ok(&[
            "add",
            topic,
            text,
            "--title",
            title,
            "--confidence",
            "verified",
        ]);
        let re = Regex::new(r"^Saved as (\d{8}T\d{6}) in (\S+)\.\n$").unwrap();
        let c = re.captures(&out).unwrap_or_else(|| panic!("{out}"));
        assert_eq!(&c[2], topic);
        c[1].to_string()
    }

    fn git(&self, args: &[&str]) -> String {
        let r = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.dir.parent().unwrap())
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        String::from_utf8(r.stdout).unwrap()
    }

    fn log(&self) -> Vec<String> {
        self.git(&["log", "--format=%s"])
            .lines()
            .map(String::from)
            .collect()
    }

    fn clean(&self) {
        assert_eq!(self.git(&["status", "--porcelain"]), "");
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.dir.join(rel)).unwrap()
    }

    fn leaf_file(&self, id: &str) -> PathBuf {
        let out = self.ok(&["show", id]);
        self.dir.join(out.lines().next().unwrap())
    }
}

#[test]
fn a_note_is_a_markdown_file_and_one_commit() {
    let n = Notes::new();
    assert!(n.ok(&[]).starts_with("No notes yet."));
    assert!(!n.dir.exists(), "reads create nothing");
    let out = n.ok(&[
        "add",
        "linguistics.syntax.minimalism",
        "Merge combines exactly two objects.",
        "--title",
        "Merge is binary",
        "--confidence",
        "reported",
        "--source",
        "https://example.org/mp",
        "--checked",
        "2026-10-13",
        "--decay",
        "slow",
    ]);
    let id = Regex::new(r"^Saved as (\d{8}T\d{6}) in linguistics\.syntax\.minimalism\.\n$")
        .unwrap()
        .captures(&out)
        .unwrap_or_else(|| panic!("{out}"))[1]
        .to_string();
    let rel = format!("linguistics/syntax/minimalism/{id}--merge-is-binary.md");
    assert_eq!(
        n.read(&rel),
        "---\nsource: https://example.org/mp\nconfidence: reported\nchecked: 2026-10-13\n\
         decay: slow\n---\n# Merge is binary\n\nMerge combines exactly two objects.\n"
    );
    assert_eq!(n.log(), ["add linguistics.syntax.minimalism"]);
    assert_eq!(n.git(&["ls-files"]), format!("{rel}\n"));
    n.clean();
    let show = n.ok(&["show", &id]);
    assert_eq!(show, format!("{rel}\n{}", n.read(&rel)));
}

#[test]
fn bodies_come_from_the_argument_or_stdin_and_are_checked() {
    let n = Notes::new();
    for args in [
        &["add", "a", "--title", "T", "--confidence", "verified"][..],
        &["add", "a", "-", "--title", "T", "--confidence", "verified"],
    ] {
        let r = n.run_in("From stdin.\n", args);
        assert_eq!(r.code, 0, "{}", r.stderr);
    }
    let bodies = n.git(&["grep", "-h", "From stdin"]);
    assert_eq!(bodies, "From stdin.\nFrom stdin.\n");
    let long = "x".repeat(LEAF_MAX + 1);
    assert!(
        n.fail(&[
            "add",
            "a",
            &long,
            "--title",
            "T",
            "--confidence",
            "verified"
        ])
        .starts_with("Too long: 1025 bytes")
    );
    assert!(
        n.fail(&["add", "a", " ", "--title", "T", "--confidence", "verified"])
            .starts_with("Empty.")
    );
    assert!(
        n.fail(&[
            "add",
            "A.b",
            "x",
            "--title",
            "T",
            "--confidence",
            "verified"
        ])
        .contains("is not a topic")
    );
    assert!(
        n.fail(&["add", "a", "x", "--title", "T"])
            .contains("--confidence")
    );
    assert!(
        n.fail(&["add", "a", "x", "--confidence", "verified"])
            .contains("--title")
    );
    assert!(
        n.fail(&["add", "a", "x", "--title", " ", "--confidence", "verified"])
            .starts_with("A title is one line")
    );
    let long_title = "t".repeat(81);
    assert!(
        n.fail(&[
            "add",
            "a",
            "x",
            "--title",
            &long_title,
            "--confidence",
            "verified"
        ])
        .starts_with("Title too long: 81 characters")
    );
    assert!(
        n.ok(&[
            "add",
            "a",
            &"x".repeat(LEAF_MAX),
            "--title",
            "T",
            "--confidence",
            "verified"
        ])
        .starts_with("Saved"),
        "the title is not counted against the body"
    );
    assert!(
        n.fail(&["add", "a", "x", "--title", "T", "--confidence", "sure"])
            .contains("verified")
    );
    assert!(
        n.fail(&[
            "add",
            "a",
            "x",
            "--title",
            "T",
            "--confidence",
            "verified",
            "--link",
            "20000101T000000"
        ])
        .contains("No note 20000101T000000")
    );
    assert_eq!(n.log().len(), 3, "failures commit nothing");
    n.clean();
}

#[test]
fn ids_are_unique_and_permanent() {
    let n = Notes::new();
    let ids: Vec<String> = (0..4).map(|i| n.add("t", &format!("claim {i}"))).collect();
    let mut sorted = ids.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "{ids:?}");
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    n.ok(&["mv", &ids[0], "u"]);
    assert!(
        n.ok(&["show", &ids[0]])
            .starts_with(&format!("u/{}--claim-0.md\n", ids[0]))
    );
}

#[test]
fn summaries_build_bottom_up_and_only_when_out_of_date() {
    let n = Notes::new();
    n.add("a.b.c", "Deep claim.");
    n.add("a.b", "Middle claim.");
    n.add("a.d", "Side claim.");
    assert_eq!(n.ok(&["build"]), "Built 4 summaries.\n");
    let order = n.fake.take();
    let at = |t: &str| order.iter().position(|o| o == t).unwrap();
    assert_eq!(order.len(), 4);
    assert!(
        at("c") < at("b") && at("b") < at("a") && at("d") < at("a"),
        "{order:?}"
    );
    let sum = n.read(&format!("a/b/{SUMMARY}"));
    let hash =
        Regex::new(r"^---\nhash: [0-9a-f]{40}\n---\nscope of b\n\ngist of b: 2 items\n$").unwrap();
    assert!(hash.is_match(&sum), "{sum}");
    assert_eq!(
        n.log().iter().filter(|m| m.starts_with("summary ")).count(),
        4
    );
    n.clean();

    assert_eq!(n.ok(&["build"]), "Nothing to build.\n");
    assert!(n.fake.take().is_empty());

    n.add("a.d", "Another side claim.");
    assert_eq!(n.ok(&["build"]), "Built 2 summaries.\n");
    assert_eq!(n.fake.take(), ["d", "a"]);
    let a = n.fake.inputs.lock().unwrap().clone();
    assert!(a.is_empty());
    n.clean();
}

#[test]
fn a_topic_holding_one_subtopic_takes_its_summary_without_a_model_call() {
    let n = Notes::new();
    n.add("linguistics.syntax.minimalism", "Merge is binary.");
    assert_eq!(n.ok(&["build"]), "Built 3 summaries.\n");
    assert_eq!(n.fake.take(), ["minimalism"]);
    assert_eq!(
        n.ok(&[]),
        "linguistics.syntax.minimalism (1): scope of minimalism\n"
    );
    let body = |t: &str| {
        let s = n.read(&format!("{t}/{SUMMARY}"));
        s.split_once("---\n")
            .unwrap()
            .1
            .split_once("---\n")
            .unwrap()
            .1
            .to_string()
    };
    for t in ["linguistics", "linguistics/syntax"] {
        assert_eq!(
            body(t),
            "scope of minimalism\n\ngist of minimalism: 1 items\n"
        );
    }
    assert_eq!(
        n.ok(&["build"]),
        "Nothing to build.\n",
        "the copies carry their hashes"
    );

    n.add("linguistics.syntax", "Move is internal merge.");
    assert_eq!(n.ok(&["build"]), "Built 2 summaries.\n");
    assert_eq!(
        n.fake.take(),
        ["syntax"],
        "a note of its own needs the model"
    );
    assert_eq!(
        n.ok(&[]),
        "linguistics.syntax (2): scope of syntax\n  linguistics.syntax.minimalism (1): scope of minimalism\n",
        "the view collapses only copies"
    );
    assert_eq!(
        body("linguistics"),
        "scope of syntax\n\ngist of syntax: 2 items\n"
    );
    n.clean();
}

#[test]
fn the_model_sees_children_and_retries_overlong_replies() {
    let n = Notes::with(Fake {
        long: true,
        ..Fake::default()
    });
    n.add("a.b", "Child claim.");
    n.ok(&[
        "add",
        "a",
        "A guess.",
        "--title",
        "T",
        "--confidence",
        "speculative",
    ]);
    n.ok(&["build"]);
    let inputs = n.fake.inputs.lock().unwrap().clone();
    assert!(
        inputs[0].ends_with("\nTopic: b\n- note [verified] Child claim.: Child claim."),
        "{}",
        inputs[0]
    );
    assert!(
        inputs[1].ends_with("\nTopic: a\n- subtopic b: scope of b | gist of b: 1 items\n- note [speculative] T: A guess."),
        "{}",
        inputs[1]
    );
    let retries = n.fake.retries.lock().unwrap().clone();
    assert_eq!(retries.len(), 2);
    assert!(
        retries[0].starts_with("The scope is 161 bytes (limit 160)"),
        "{}",
        retries[0]
    );
    assert!(n.read(&format!("a/{SUMMARY}")).contains("\nscope of a\n"));
}

#[test]
fn invalid_notes_stay_out_of_summaries() {
    let n = Notes::new();
    let old = n.add("t", "The old claim.");
    n.add("t", "The new claim.");
    let f = n.leaf_file(&old);
    let text = fs::read_to_string(&f).unwrap();
    fs::write(
        &f,
        text.replace("---\nconfidence", "---\ninvalid_at: 2026-10-13\nconfidence"),
    )
    .unwrap();
    n.ok(&["build"]);
    let inputs = n.fake.inputs.lock().unwrap().clone();
    assert!(inputs[0].contains("The new claim.") && !inputs[0].contains("The old claim."));
    assert!(n.ok(&["show", &old]).contains("invalid_at: 2026-10-13\n"));
    n.clean();
}

#[test]
fn view_zoom_grep_and_show_read_the_tree() {
    let n = Notes::new();
    let merge = n.add_titled(
        "linguistics.syntax",
        "Merge is binary",
        "Merge takes two objects.",
    );
    let deco = n.add_titled(
        "physics.qm.decoherence",
        "Decoherence explains collapse",
        "Entanglement with the environment makes collapse apparent.",
    );
    assert_eq!(
        n.ok(&["view"]),
        format!(
            "linguistics (1): {PLACEHOLDER}\n  linguistics.syntax (1): {PLACEHOLDER}\n\
             physics (1): {PLACEHOLDER}\n  physics.qm (1): {PLACEHOLDER}\n    physics.qm.decoherence (1): {PLACEHOLDER}\n"
        )
    );
    n.ok(&["build"]);
    assert_eq!(n.ok(&[]), n.ok(&["view"]));
    assert_eq!(
        n.ok(&[]),
        "linguistics.syntax (1): scope of syntax\nphysics.qm.decoherence (1): scope of decoherence\n",
        "chains of copied summaries collapse into one line"
    );
    assert_eq!(
        n.ok(&["zoom", "physics.qm"]),
        "physics.qm (1): scope of decoherence\ngist of decoherence: 1 items\n\n\
         physics.qm.decoherence (1): scope of decoherence\ngist of decoherence: 1 items\n"
    );
    assert_eq!(
        n.ok(&["zoom", "linguistics.syntax"]),
        format!(
            "linguistics.syntax (1): scope of syntax\ngist of syntax: 1 items\n\n{merge} [verified] Merge is binary\n"
        )
    );
    assert!(
        n.fail(&["zoom", "chemistry"])
            .starts_with("No topic chemistry.")
    );
    assert_eq!(
        n.ok(&["grep", "COLLAPSE"]),
        "No match.\n",
        "a capital makes it case-sensitive"
    );
    assert_eq!(
        n.ok(&["grep", "decoherence"]),
        format!("physics › qm › decoherence › {deco}: Decoherence explains collapse\n"),
        "summaries above a deeper hit are left out"
    );
    assert_eq!(
        n.ok(&["grep", "environment"]),
        format!("physics › qm › decoherence › {deco}: Decoherence explains collapse\n"),
        "a body match shows the title"
    );
    assert_eq!(
        n.ok(&["grep", "syntax|merge"]),
        format!("linguistics › syntax › {merge}: Merge is binary\n")
    );
    assert_eq!(
        n.ok(&["grep", "scope"]),
        "linguistics › syntax: scope of syntax gist of syntax: 1 items\n\
         physics › qm › decoherence: scope of decoherence gist of decoherence: 1 items\n"
    );
    assert_eq!(n.ok(&["grep", "-F", "a.p"]), "No match.\n");
    assert!(n.fail(&["show", "merge"]).contains("is not a note id"));
    assert!(n.fail(&["show", "20000101T000000"]).starts_with("No note"));
}

#[test]
fn the_view_keeps_to_its_budget() {
    let n = Notes::new();
    let deep = "y".repeat(40);
    for i in 0..60 {
        n.add(&format!("t{i:02}.{deep}.{deep}"), "x");
    }
    let v = n.ok(&[]);
    assert!(v.len() <= ai_tools::notes::VIEW_MAX + 100, "{}", v.len());
    assert!(v.contains("t00 (1)") && v.contains(&format!("  t59.{deep} (1)")));
    assert!(
        !v.contains(&format!("{deep}.{deep}")),
        "the third level is hidden"
    );
    assert!(
        v.ends_with("Deeper topics below. Open one: ai notes zoom <topic>\n"),
        "{v}"
    );
}

#[test]
fn mv_moves_notes_and_topics_and_resummarises_both_sides() {
    let n = Notes::new();
    let a = n.add("x.y", "First.");
    n.add("x.y", "Second.");
    n.add("x.z.w", "Deep.");
    n.ok(&["build"]);
    n.fake.take();

    assert_eq!(n.ok(&["mv", &a, "q"]), format!("Moved {a} to q.\n"));
    assert!(n.leaf_file(&a).starts_with(n.dir.join("q")));
    n.ok(&["build"]);
    let mut t = n.fake.take();
    t.sort();
    assert_eq!(t, ["q", "x", "y"]);

    n.ok(&["mv", "x.z", "r.z"]);
    assert!(!n.dir.join("x/z").exists());
    n.ok(&["build"]);
    assert!(
        n.fake.take().is_empty(),
        "the moved summaries stay valid; r and x each take their one child's"
    );
    assert!(n.read(&format!("r/{SUMMARY}")).contains("\nscope of w\n"));
    assert!(n.read(&format!("x/{SUMMARY}")).contains("\nscope of y\n"));

    n.ok(&["mv", "x.y", "s"]);
    assert!(
        !n.dir.join("x").exists(),
        "emptied topics go, summary and all"
    );
    assert!(n.fail(&["mv", "s", "s.t"]).contains("into itself"));
    assert!(n.fail(&["mv", "s", "q"]).starts_with("q exists."));
    assert!(n.fail(&["mv", "nope", "q"]).starts_with("No topic nope."));
    assert!(n.fail(&["mv", &a, "q"]).contains("already"));
    assert_eq!(n.log()[0], "mv x.y s");
    n.ok(&["build"]);
    n.clean();
}

#[test]
fn links_are_weak_references_that_do_not_touch_summaries() {
    let n = Notes::new();
    let a = n.add("p", "A claim.");
    let b = n.add("q.r", "Related claim.");
    let hub = n.add("p", &format!("Hub: see [[{b}]]."));
    n.ok(&["build"]);
    n.fake.take();
    assert_eq!(n.ok(&["link", &a, &b]), format!("Linked {a} to {b}.\n"));
    assert_eq!(
        n.ok(&["link", &a, &b]),
        format!("{a} links to {b} already.\n")
    );
    assert!(n.fail(&["link", &a, &a]).contains("itself"));
    assert!(
        n.fail(&["link", &a, "20000101T000000"])
            .starts_with("No note")
    );
    let show = n.ok(&["show", &a]);
    assert!(
        show.contains(&format!(
            "\nlinks: [{b}]\n---\n# A claim.\n\nA claim.\n\nLinks:\nq › r › {b}: Related claim.\n"
        )),
        "{show}"
    );
    assert!(
        n.ok(&["show", &hub])
            .ends_with(&format!("Links:\nq › r › {b}: Related claim.\n"))
    );
    assert_eq!(n.ok(&["build"]), "Nothing to build.\n");

    let old = n.leaf_file(&a);
    let text = fs::read_to_string(&old).unwrap();
    fs::write(&old, text.replace("# A claim.", "# Renamed by hand")).unwrap();
    assert!(
        n.ok(&["show", &a]).contains("# Renamed by hand"),
        "found by id"
    );
    n.ok(&["link", &a, &hub]);
    assert!(!old.exists());
    assert_eq!(
        n.leaf_file(&a),
        n.dir.join(format!("p/{a}--renamed-by-hand.md")),
        "a write renames the file after its title, keeping the id"
    );
    assert_eq!(n.log().iter().filter(|m| m.starts_with("link ")).count(), 2);
    n.clean();
}

#[test]
fn a_dir_with_other_files_is_refused() {
    let n = Notes::new();
    fs::create_dir_all(&n.dir).unwrap();
    fs::write(n.dir.join("x"), "").unwrap();
    assert!(
        n.fail(&["add", "a", "b", "--title", "T", "--confidence", "verified"])
            .contains("holds files but no notes")
    );
}

#[test]
fn parallel_adds_lose_nothing() {
    let n = Notes::new();
    std::thread::scope(|s| {
        for i in 0..8 {
            let n = &n;
            s.spawn(move || {
                let r = run_rt(
                    &n.dir,
                    &runtime(&n.fake, ""),
                    &[
                        "add",
                        "t",
                        &format!("claim {i}"),
                        "--title",
                        "T",
                        "--confidence",
                        "verified",
                    ],
                );
                assert_eq!(r.code, 0, "{}", r.stderr);
            });
        }
    });
    assert_eq!(n.log().len(), 8);
    assert_eq!(n.git(&["ls-files"]).lines().count(), 8);
    n.clean();
}
