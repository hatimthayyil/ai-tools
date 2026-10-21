use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

use ai_tools::{
    files::summarise::{Child, FLOOR, PROMPT, Summary, accept, dir, file},
    memory::{Backend, CALLS, ClaudeCode, Conversation, Spend, TRIES, Usage},
};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use tempfile::TempDir;

type Reply = dyn Fn(&str) -> String + Send + Sync;

/// A model that answers each message by `reply`, keeping every conversation's messages.
struct Fake {
    reply: Box<Reply>,
    convs: Mutex<Vec<Arc<Mutex<Vec<String>>>>>,
}

impl Fake {
    fn new(reply: impl Fn(&str) -> String + Send + Sync + 'static) -> Arc<Fake> {
        Arc::new(Fake {
            reply: Box::new(reply),
            convs: Mutex::new(Vec::new()),
        })
    }

    /// The messages of each conversation opened, in order.
    fn convs(&self) -> Vec<Vec<String>> {
        let convs = self.convs.lock().unwrap();
        convs.iter().map(|c| c.lock().unwrap().clone()).collect()
    }
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
        assert_eq!(system, PROMPT);
        let said = Arc::new(Mutex::new(Vec::new()));
        self.0.convs.lock().unwrap().push(said.clone());
        Ok(Box::new(Conv {
            fake: self.0.clone(),
            said,
        }))
    }
}

struct Conv {
    fake: Arc<Fake>,
    said: Arc<Mutex<Vec<String>>>,
}

impl Conversation for Conv {
    fn say(&mut self, message: &[ai_tools::memory::Block]) -> Result<String> {
        let [b] = message else {
            panic!("one block: {message:?}")
        };
        self.said.lock().unwrap().push(b.text.clone());
        Ok((self.fake.reply)(&b.text))
    }

    fn session(&self) -> String {
        "fake-session".into()
    }

    fn used(&self) -> Usage {
        Usage {
            input: 100,
            output: 10,
            cost: 0.01,
            ..Usage::default()
        }
    }
}

/// The bytes the last "at most N bytes" in `message` asks for.
fn asked(message: &str) -> usize {
    Regex::new(r"at most (\d+) bytes")
        .unwrap()
        .captures_iter(message)
        .last()
        .expect("an ask")[1]
        .parse()
        .unwrap()
}

/// A reply of exactly the bytes `message` asks for, times `scale`.
fn sized(scale: f64) -> impl Fn(&str) -> String + Send + Sync {
    move |m| "s".repeat((asked(m) as f64 * scale) as usize)
}

fn children(sizes: &[usize]) -> Vec<Child> {
    sizes
        .iter()
        .enumerate()
        .map(|(i, &n)| Child {
            path: format!("src/chat/c{i}.rs"),
            summary: format!("{i}").repeat(n),
        })
        .collect()
}

#[test]
fn a_file_is_summarised_whole_in_one_call_at_a_tenth_of_its_bytes() {
    let fake = Fake::new(|_| "  It rolls up a thread.\nSee src/chat/rollup.rs:2.\n\n".into());
    let text = "use std::fmt;\n".repeat(1_000);
    let mut spent = Spend::default();
    let s = file(
        &Model(fake.clone()),
        "src/chat/rollup.rs",
        &text,
        &mut spent,
    )
    .unwrap();
    assert_eq!(
        s,
        Summary {
            text: "It rolls up a thread.\nSee src/chat/rollup.rs:2.".into(),
            who: ai_tools::memory::Who {
                agent: "fake-harness".into(),
                model: "fake".into(),
                session: "fake-session".into(),
            },
        }
    );
    let convs = fake.convs();
    assert_eq!(convs.len(), 1);
    let [m] = &convs[0][..] else {
        panic!("one call: {convs:?}")
    };
    assert!(m.starts_with("<file path=\"src/chat/rollup.rs\">\n1\tuse std::fmt;\n2\tuse"));
    assert!(m.contains("1000\tuse std::fmt;\n</file>\n"));
    assert!(m.ends_with(
        "Summarise the file src/chat/rollup.rs above, 14000 bytes, in at most 1400 bytes."
    ));
    assert_eq!((spent.requests, spent.retries), (1, 0));
}

#[test]
fn a_small_file_is_asked_for_the_floor() {
    let fake = Fake::new(sized(1.0));
    let s = file(&Model(fake.clone()), "a.md", "# A\n", &mut Spend::default()).unwrap();
    assert_eq!(s.text.len(), FLOOR);
    assert_eq!(asked(&fake.convs()[0][0]), FLOOR);
}

#[test]
fn a_summary_over_a_quarter_more_than_asked_is_retried_in_its_conversation() {
    let fake = Fake::new(|m| {
        if m.starts_with("That summary is") {
            "r".repeat(asked(m))
        } else {
            "f".repeat(accept(asked(m)) + 1)
        }
    });
    let text = "x".repeat(20_000);
    let mut spent = Spend::default();
    let s = file(&Model(fake.clone()), "big.txt", &text, &mut spent).unwrap();
    let convs = fake.convs();
    assert_eq!(convs.len(), 1, "the retry stays in the conversation");
    assert_eq!(convs[0].len(), 2);
    assert!(convs[0][1].starts_with("That summary is 2501 bytes, too long."));
    assert_eq!(s.text, "r".repeat(1_599));
    assert_eq!((spent.requests, spent.retries), (2, 1));

    let accepted = Fake::new(|m| "f".repeat(accept(asked(m))));
    file(
        &Model(accepted.clone()),
        "big.txt",
        &text,
        &mut Spend::default(),
    )
    .unwrap();
    assert_eq!(accepted.convs()[0].len(), 1, "a quarter more is accepted");
}

#[test]
fn a_summary_still_too_long_after_every_try_fails() {
    let fake = Fake::new(|_| "f".repeat(10_000));
    let mut spent = Spend::default();
    let e = file(&Model(fake.clone()), "a.rs", "fn a() {}\n", &mut spent).unwrap_err();
    assert_eq!(
        e.to_string(),
        format!("a.rs: 10000 bytes after {TRIES} tries; this summary holds at most 640")
    );
    assert_eq!(fake.convs()[0].len(), TRIES);
    assert_eq!(spent.requests, TRIES as u64);
}

#[test]
fn a_directory_halves_in_passes_of_one_conversation_down_to_its_childrens_mean() {
    let fake = Fake::new(sized(1.0));
    let kids = children(&[2_000, 2_000, 2_000, 2_000, 2_000, 2_000, 2_000, 2_000]);
    let mut spent = Spend::default();
    let s = dir(&Model(fake.clone()), "src/chat", &kids, &mut spent).unwrap();
    assert_eq!(s.text.len(), 2_000);
    assert_eq!(s.who.session, "fake-session");
    let convs = fake.convs();
    assert_eq!(convs.len(), 1, "every pass in one conversation");
    let passes = &convs[0];
    let asks: Vec<usize> = passes.iter().map(|m| asked(m)).collect();
    assert_eq!(asks, [8_000, 4_000, 2_000]);
    let first = &passes[0];
    for (i, c) in kids.iter().enumerate() {
        assert!(first.contains(&format!(
            "<summary path=\"src/chat/c{i}.rs\">\n{}\n</summary>\n",
            c.summary
        )));
    }
    assert!(first.ends_with(
        "Summarise the directory src/chat from the summaries of its 8 children above, 16000 bytes in all, in at most 8000 bytes."
    ));
    let second = &passes[1];
    assert_eq!(
        second,
        &format!(
            "<summary path=\"src/chat\">\n{}\n</summary>\nCondense this summary of the directory src/chat, 8000 bytes, to at most 4000 bytes.",
            "s".repeat(8_000)
        )
    );
    assert!(
        !second.contains("c0.rs"),
        "a later pass takes only the pass before"
    );
    assert_eq!(spent.requests, 3);
}

#[test]
fn passes_halve_what_the_last_one_wrote_and_end_once_it_fits_the_target() {
    let fake = Fake::new(sized(0.3));
    let kids = children(&[4_000, 4_000, 4_000, 4_000]);
    let s = dir(&Model(fake.clone()), "docs", &kids, &mut Spend::default()).unwrap();
    let asks: Vec<usize> = fake.convs()[0].iter().map(|m| asked(m)).collect();
    // 16000 in, target 4000: asked 8000, wrote 2400, already within the target.
    assert_eq!(asks, [8_000]);
    assert_eq!(s.text.len(), 2_400);

    let fake = Fake::new(sized(0.9));
    let kids = children(&[3_000; 10]);
    dir(&Model(fake.clone()), "docs", &kids, &mut Spend::default()).unwrap();
    let asks: Vec<usize> = fake.convs()[0].iter().map(|m| asked(m)).collect();
    // 30000 in, target 3000: 15000 -> wrote 13500 -> 6750 -> wrote 6075 -> 3037 -> wrote 2733.
    assert_eq!(asks, [15_000, 6_750, 3_037]);
}

#[test]
fn a_directory_of_small_children_takes_one_pass_at_the_floor() {
    let fake = Fake::new(sized(1.0));
    let s = dir(
        &Model(fake.clone()),
        ".",
        &children(&[100, 300]),
        &mut Spend::default(),
    )
    .unwrap();
    assert_eq!(fake.convs()[0].len(), 1);
    assert_eq!(asked(&fake.convs()[0][0]), FLOOR);
    assert_eq!(s.text.len(), FLOOR);
}

#[test]
fn a_directory_without_children_is_an_error() {
    let fake = Fake::new(sized(1.0));
    let e = dir(&Model(fake.clone()), "empty", &[], &mut Spend::default()).unwrap_err();
    assert_eq!(e.to_string(), "empty has no summarised children.");
    assert!(fake.convs().is_empty());
}

fn script(path: &std::path::Path, text: &str) {
    let mut sh = Command::new("sh")
        .args(["-c", r#"cat >"$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    sh.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    assert!(sh.wait().unwrap().success());
}

#[test]
fn a_directorys_passes_run_in_one_claude_session_kept_whole_under_calls() {
    let tmp = TempDir::new().unwrap();
    let program = tmp.path().join("claude");
    // Eight 256-byte children: pass 1 asks 1024 and gets 700, over the 512 target's 640;
    // pass 2 asks 512 and gets 300.
    script(
        &program,
        r#"#!/bin/sh
while [ "$1" != --session-id ]; do shift; done
n=0
while read -r line; do
  n=$((n+1))
  if [ $n = 1 ]; then k=700; else k=300; fi
  s=$(head -c $k /dev/zero | tr '\0' s)
  printf '{"type":"result","is_error":false,"result":"%s","usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":90,"output_tokens":3},"total_cost_usd":0.0%s,"session_id":"%s"}\n' "$s" $n "$2"
done
"#,
    );
    let calls = tmp.path().join(CALLS);
    let c = ClaudeCode::compactor_at(program.into(), "sonnet", &calls).unwrap();
    let mut spent = Spend::default();
    let s = dir(&c, "src", &children(&[256; 8]), &mut spent).unwrap();
    assert_eq!(s.text, "s".repeat(300));
    assert_eq!(
        (s.who.agent.as_str(), s.who.model.as_str()),
        ("claude-code", "sonnet")
    );
    assert_eq!(spent.requests, 2);
    assert_eq!(spent.all.cache_read, 180);
    let files: Vec<_> = fs::read_dir(&calls)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "one session: {files:?}");
    assert_eq!(
        files[0].file_stem().unwrap().to_str().unwrap(),
        s.who.session
    );
    let record = fs::read_to_string(&files[0]).unwrap();
    let lines: Vec<Value> = record
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["type"], "ai.call");
    assert_eq!(lines[0]["system"], PROMPT);
    let sent: Vec<&Value> = lines.iter().filter(|l| l["type"] == "ai.sent").collect();
    assert_eq!(sent.len(), 2);
    let text = |v: &Value| {
        v["line"]["message"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(text(sent[0]).ends_with("in at most 1024 bytes."));
    assert!(text(sent[1]).ends_with("700 bytes, to at most 512 bytes."));
}
