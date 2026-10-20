use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

use ai_tools::memory::{Coord, Message, REF, Store};
use tempfile::TempDir;

const ISOLATE: [&str; 20] = [
    "AI_MEMORY_DIR",
    "AI_NOTES_DIR",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "AI_AGENT",
    "AI_MODEL",
    "AI_SESSION",
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "GIT_DIR",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "EMAIL",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
];

struct Sandbox {
    tmp: TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("home")).unwrap();
        Sandbox { tmp }
    }

    fn path(&self) -> &Path {
        self.tmp.path()
    }

    fn store(&self) -> PathBuf {
        self.path().join("m")
    }

    fn isolate(&self, c: &mut Command) {
        let home = self.path().join("home");
        c.env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", self.path().parent().unwrap());
        for k in ISOLATE {
            c.env_remove(k);
        }
        c.env("AI_MEMORY_SUMMARISE", "0")
            .env("AI_NOTES_SUMMARISE", "0");
    }

    fn cmd(&self, cwd: &Path, store: Option<&Path>) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ai"));
        self.isolate(&mut c);
        c.current_dir(cwd).arg("memory");
        if let Some(d) = store {
            c.env("AI_MEMORY_DIR", d);
        }
        c
    }

    fn ai(&self, args: &[&str]) -> Output {
        self.ai_at(&self.store(), args)
    }

    fn ai_at(&self, d: &Path, args: &[&str]) -> Output {
        self.cmd(self.path(), Some(d)).args(args).output().unwrap()
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let mut c = Command::new("git");
        self.isolate(&mut c);
        let r = c
            .current_dir(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@t"])
            .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
            .args(args)
            .output()
            .unwrap();
        assert!(r.status.success(), "git {args:?}: {}", text(&r.stderr));
        text(&r.stdout).trim_end().to_string()
    }

    fn store_git(&self, args: &[&str]) -> String {
        self.git_at(&self.store(), args)
    }

    fn git_at(&self, d: &Path, args: &[&str]) -> String {
        let dir = format!("--git-dir={}", d.display());
        self.git(self.path(), &[&[dir.as_str()], args].concat())
    }

    fn memories(&self) -> Vec<Message> {
        let st = Store::open(&self.store()).unwrap();
        let snap = st.snapshot().unwrap();
        let mut out = Vec::new();
        snap.scan(u64::MAX, |_, m| {
            out.push(m);
            Ok(())
        })
        .unwrap();
        out
    }

    /// Installs `body` as a bash `claude` in `bin/` and returns a `PATH` that finds it first.
    fn fake_claude(&self, body: &str) -> String {
        let path = std::env::var("PATH").unwrap();
        let bash = std::env::split_paths(&path)
            .map(|d| d.join("bash"))
            .find(|p| p.is_file())
            .expect("bash on PATH");
        let bin = self.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let src = self.path().join("claude.sh");
        fs::write(&src, format!("#!{}\n{body}", bash.display())).unwrap();
        let claude = bin.join("claude");
        assert!(
            Command::new("cp")
                .arg(&src)
                .arg(&claude)
                .status()
                .unwrap()
                .success()
        );
        fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        format!("{}:{path}", bin.display())
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn defaults_to_the_home_dir() {
    let s = Sandbox::new();
    let r = s.cmd(s.path(), None).arg("init").output().unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    assert!(text(&r.stdout).starts_with("Created ~/.ai/memory: your memory."));
    let d = s.path().join("home/.ai/memory");
    assert_eq!(
        s.git(
            s.path(),
            &[
                &format!("--git-dir={}", d.display()),
                "rev-parse",
                "--is-bare-repository"
            ]
        ),
        "true"
    );
    let r = s.cmd(s.path(), None).arg("view").output().unwrap();
    assert!(r.status.success() && text(&r.stdout).contains("You are awake."));
}

#[test]
fn exit_codes_and_streams() {
    let s = Sandbox::new();
    let d = s.store();
    let r = s.ai(&["view"]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("No memory at") && text(&r.stderr).contains("ai memory init"));
    assert!(!d.exists());

    assert!(s.ai(&["init"]).status.success());
    let r = s.ai(&["view"]);
    assert!(r.status.success() && text(&r.stdout).contains("No memories yet"));
    assert!(!s.ai(&["zoom"]).status.success());

    assert!(s.ai(&["note", "memory 0"]).status.success());
    let r = s.ai(&["zoom", "0+2"]);
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(text(&r.stderr), "No line 0+2.\n");
    assert!(!s.ai(&["frobnicate"]).status.success());
}

#[test]
fn parallel_processes_lose_nothing() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    let p = 16;
    let kids: Vec<_> = (0..p)
        .map(|i| {
            s.cmd(s.path(), Some(&s.store()))
                .args(["note", &format!("parallel note {i}")])
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut k in kids {
        assert!(k.wait().unwrap().success());
    }
    let all = s.memories();
    let mut texts: Vec<_> = all.iter().map(|m| m.text.clone()).collect();
    texts.sort();
    let mut want: Vec<_> = (0..p).map(|i| format!("parallel note {i}")).collect();
    want.sort();
    assert_eq!(texts, want);
    assert!(all.windows(2).all(|w| w[0].ts <= w[1].ts));
    let log = s.store_git(&["log", "--format=%s", REF]);
    assert_eq!(log.lines().filter(|l| *l == "note").count(), p);
}

#[test]
fn the_model_comes_from_the_session_transcript() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    let dir = s.path().join("home/.claude/projects/-started-elsewhere");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("sess-9.jsonl"),
        concat!(
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","role":"assistant"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"run it"}}"#,
            "\n",
        ),
    )
    .unwrap();
    let r = s
        .cmd(s.path(), Some(&s.store()))
        .args(["note", "the model is found"])
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_SESSION_ID", "sess-9")
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    assert_eq!(s.memories()[0].who.model, "claude-opus-5-5");
}

#[test]
fn provenance_comes_from_the_cwd_and_env() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    let work = s.path().join("work");
    let sub = work.join("deep/er");
    fs::create_dir_all(&sub).unwrap();
    s.git(&work, &["init", "-q", "-b", "trunk"]);
    s.git(&work, &["commit", "-q", "--allow-empty", "-m", "c"]);
    s.git(
        &work,
        &["remote", "add", "origin", "git@github.com:acme/widget.git"],
    );
    let head = s.git(&work, &["rev-parse", "--short=12", "HEAD"]);

    let r = s
        .cmd(&sub, Some(&s.store()))
        .args(["note", "noted inside a repo"])
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_SESSION_ID", "sess-1")
        .env("AI_MODEL", "big model")
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    let r = s
        .cmd(s.path(), Some(&s.store()))
        .args(["note", "noted outside any repo"])
        .env("CLAUDECODE", "1")
        .env("AI_AGENT", "pi")
        .env("AI_SESSION", "s2")
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));

    let m = s.memories();
    let (a, b) = (&m[0], &m[1]);
    assert_eq!(
        (
            a.place.repo.as_str(),
            a.place.head.as_str(),
            a.place.branch.as_str()
        ),
        ("acme/widget", head.as_str(), "trunk")
    );
    assert_eq!(
        (
            a.who.agent.as_str(),
            a.who.model.as_str(),
            a.who.session.as_str()
        ),
        ("claude-code", "big_model", "sess-1")
    );
    assert_eq!(
        (
            b.place.repo.as_str(),
            b.place.head.as_str(),
            b.place.branch.as_str()
        ),
        ("", "", "")
    );
    assert_eq!(
        (
            b.who.agent.as_str(),
            b.who.model.as_str(),
            b.who.session.as_str()
        ),
        ("pi", "", "s2")
    );
    let root = s.git(&work, &["rev-list", "--max-parents=0", "HEAD"]);
    assert_eq!(
        (a.place.root.as_str(), b.place.root.as_str()),
        (root.as_str(), "")
    );
    assert_eq!(
        s.git(&work, &["config", "ai.root"]),
        root,
        "cached per clone"
    );
    assert!(a.ts.len() == 16 && a.ts.ends_with('Z') && a.ts <= b.ts);

    s.git(&work, &["remote", "remove", "origin"]);
    s.cmd(&work, Some(&s.store()))
        .args(["note", "noted in a repo with no remote"])
        .output()
        .unwrap();
    assert_eq!(s.memories()[2].place.repo, "work");

    let show = text(&s.ai(&["show", "0"]).stdout);
    for f in [
        "kind    note\n".into(),
        format!("origin  {}\n", a.origin),
        "repo    acme/widget\n".into(),
        format!("root    {root}\n"),
        format!("head    {head}\n"),
        "branch  trunk\n".into(),
        "agent   claude-code\n".into(),
        "model   big_model\n".into(),
        "session sess-1\n".into(),
        "text    noted inside a repo\n".into(),
    ] {
        assert!(show.contains(&f), "{f:?} missing from:\n{show}");
    }
    let grep = |args: &[&str]| -> String {
        let r = s.ai(&[&["grep", "noted"], args].concat());
        assert!(r.status.success(), "{}", text(&r.stderr));
        text(&r.stdout)
    };
    let first = |args: &[&str]| grep(args).lines().take(2).collect::<Vec<_>>().join("\n");
    let (ts, date) = (
        &a.ts,
        format!("{}-{}-{}", &a.ts[..4], &a.ts[4..6], &a.ts[6..8]),
    );
    assert_eq!(first(&[]), format!("{date}\n0|noted inside a repo"));
    assert_eq!(
        first(&["--time", "-o", "origin,repo,kind"]),
        format!(
            "{date}\n0|{}:{}|{}|acme/widget|note|noted inside a repo",
            &ts[9..11],
            &ts[11..13],
            a.origin
        )
    );
    let heads = |args: &[&str]| -> Vec<String> {
        grep(args)
            .lines()
            .filter(|l| *l != date)
            .map(|l| l.split(['|', ' ']).next().unwrap().to_string())
            .collect()
    };
    assert_eq!(heads(&["--repo", "acme/widget"]), ["0", "1"]);
    assert_eq!(heads(&["--repo", "work"]), ["2", "1"]);
    assert_eq!(heads(&["--agent", "pi"]), ["1", "1"]);
    assert_eq!(heads(&["--session", "SESS-1"]), ["0", "1"]);
    assert_eq!(heads(&["--origin", &a.origin]), ["0", "1", "2", "3"]);
    assert_eq!(grep(&["--origin", "NOPE"]), "No match.\n");
    assert_eq!(heads(&["--kind", "note"]), ["0", "1", "2", "3"]);
    assert_eq!(
        heads(&["--since", "2000-01-01", "--agent", "claude-code"]),
        ["0", "1"]
    );
    assert_eq!(grep(&["--agent", "pi", "--repo", "work"]), "No match.\n");
    assert_eq!(grep(&["--since", "2999-01-01"]), "No match.\n");
}

#[test]
fn every_mutation_is_one_named_commit() {
    let s = Sandbox::new();
    s.ai(&["init"]);
    assert_eq!(s.store_git(&["for-each-ref", "--format=%(refname)"]), "");
    let seed = s.path().join("seed.txt");
    fs::write(&seed, "2020-01-01 an imported memory\n").unwrap();
    s.ai(&["import", seed.to_str().unwrap()]);
    s.ai(&["note", "a noted memory"]);
    s.ai(&["view"]);
    s.ai(&["build"]);
    assert_eq!(
        s.store_git(&["log", "--format=%s|%cn <%ce>", REF]),
        "note|ai memory <ai@localhost>\n\
         import|ai memory <ai@localhost>"
    );
    assert_eq!(
        s.store_git(&["ls-tree", "-r", "--name-only", REF]),
        "format\nlog/0000/00\nlog/0000/01\ntree/0/0000/00\ntree/1/0000/00"
    );
    assert_eq!(s.store_git(&["for-each-ref", "--format=%(refname)"]), REF);
    s.store_git(&["fsck", "--strict", "--no-dangling"]);
}

fn listing(root: &Path, skip: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        for e in fs::read_dir(p).unwrap() {
            let p = e.unwrap().path();
            if p == skip {
                continue;
            }
            let m = fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                stack.push(p.clone());
            }
            out.insert(p, (m.len(), m.modified().unwrap()));
        }
    }
    out
}

#[test]
fn nothing_is_written_outside_the_store_but_the_root_cache() {
    let s = Sandbox::new();
    let work = s.path().join("work");
    fs::create_dir(&work).unwrap();
    s.git(&work, &["init", "-q"]);
    s.git(&work, &["commit", "-q", "--allow-empty", "-m", "c"]);
    let seed = s.path().join("seed.txt");
    fs::write(&seed, "2020-01-01 an imported memory\n").unwrap();
    let before = listing(s.path(), &s.store());
    let d = s.store();
    for args in [
        vec!["init"],
        vec!["import", seed.to_str().unwrap()],
        vec!["note", "first"],
        vec!["note", "second"],
        vec!["view"],
        vec!["config", "PART_LINES=4"],
        vec!["grep", "first", "-t"],
        vec!["zoom", "0+2"],
        vec!["show", "0+2"],
    ] {
        let r = s.cmd(&work, Some(&d)).args(&args).output().unwrap();
        assert!(
            r.status.code().is_some_and(|c| c <= 1) && text(&r.stderr).is_empty(),
            "{args:?}: {}",
            text(&r.stderr)
        );
    }
    let cached = [work.join(".git"), work.join(".git/config")];
    let outside = |mut l: BTreeMap<PathBuf, _>| {
        l.retain(|p, _| !cached.contains(p));
        l
    };
    assert_eq!(outside(listing(s.path(), &d)), outside(before));
    let root = s.git(&work, &["rev-list", "--max-parents=0", "HEAD"]);
    assert_eq!(
        s.git(&work, &["config", "ai.root"]),
        root,
        "a note caches its repo's root"
    );
    let mut top: Vec<_> = fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    top.sort();
    assert_eq!(
        top,
        [
            "HEAD",
            "config",
            "description",
            "hooks",
            "info",
            "objects",
            "refs",
            "usage.jsonl"
        ]
    );
}

fn usage(s: &Sandbox) -> Vec<serde_json::Value> {
    fs::read_to_string(s.store().join("usage.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn every_command_logs_one_usage_line() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    assert!(!s.store().join("usage.jsonl").exists());
    let work = s.path().join("work");
    fs::create_dir(&work).unwrap();
    s.git(&work, &["init", "-q", "-b", "trunk"]);
    s.git(&work, &["commit", "-q", "--allow-empty", "-m", "c"]);
    s.git(
        &work,
        &["remote", "add", "origin", "git@github.com:acme/widget.git"],
    );
    let by = |args: &[&str], session: &str| {
        s.cmd(&work, Some(&s.store()))
            .args(args)
            .env("AI_AGENT", "pi")
            .env("AI_MODEL", "m1")
            .env("AI_SESSION", session)
            .output()
            .unwrap()
    };
    for (args, session) in [
        (&["view"][..], "s1"),
        (&["note", "a noted memory"], "s1"),
        (&["note", "two\nlines"], "s1"),
        (&["zoom", "0", "--time", "-o", "repo"], "s1"),
        (&["zoom", "9"], "s1"),
        (&["grep", "it's", "-t", "-m", "3", "--before", "0"], "s1"),
        (&["show", "0"], "s2"),
    ] {
        by(args, session);
    }
    let log = usage(&s);
    let fields = |k: &str| -> Vec<String> {
        log.iter()
            .map(|u| match &u[k] {
                serde_json::Value::String(v) => v.clone(),
                v => v.to_string(),
            })
            .collect()
    };
    assert_eq!(
        fields("cmd"),
        ["view", "note", "note", "zoom", "zoom", "grep", "show"]
    );
    assert_eq!(
        fields("args"),
        [
            "[]",
            "[]",
            "[]",
            r#"["0","--time","-o","repo"]"#,
            r#"["9"]"#,
            r#"["it's","-t","-m","3","--before","0"]"#,
            r#"["0"]"#
        ]
    );
    assert_eq!(
        fields("ok"),
        ["true", "true", "false", "true", "false", "true", "true"]
    );
    assert_eq!(log[4]["error"], "No line 9.");
    assert!(log[2]["error"].as_str().unwrap().contains("one line"));
    assert!(log[0].get("error").is_none());
    assert_eq!(log[1]["lines"], 1);
    assert_eq!(log[1]["bytes"], "Saved as 0.\n".len());
    assert_eq!(log[6]["lines"], 12);
    assert_eq!(log[4]["bytes"], 0);
    for (k, v) in [
        ("agent", "pi"),
        ("model", "m1"),
        ("repo", "acme/widget"),
        ("branch", "trunk"),
    ] {
        assert!(fields(k).iter().all(|f| f == v), "{k}: {:?}", fields(k));
    }
    assert_eq!(fields("session")[5..], ["s1", "s2"]);
    let ts = |t: &String| t.len() == 16 && t[8..9] == *"T" && t.ends_with('Z');
    assert!(fields("ts").iter().all(ts), "{:?}", fields("ts"));

    let r = s.ai(&["stats", "--since", "2020-01-01", "--by", "agent"]);
    assert_eq!(
        text(&r.stdout),
        "Since 2020-01-01, UTC.\n\
         agent  view  note  zoom  grep  show\n\
         pi        1     1     1     1     1\n\
         all       1     1     1     1     1\n\
         \n\
         2 sessions, 1 woke.\n\
         Per woken session: 1.0 notes, 1.0 zooms, 1.0 greps, 0.0 shows.\n\
         Of these, 100% noted, 100% zoomed, 100% grepped, 0% showed.\n\
         Failed: 1 note, 1 zoom.\n",
        "{}",
        text(&r.stderr)
    );
    assert_eq!(usage(&s).last().unwrap()["cmd"], "stats");
    assert_eq!(
        usage(&s).last().unwrap()["args"],
        serde_json::json!(["--since", "2020-01-01", "--by", "agent"])
    );
}

#[test]
fn parallel_reads_log_whole_lines() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    assert!(s.ai(&["note", "one memory"]).status.success());
    let p = 16;
    let pattern = "p".repeat(10_000);
    let kids: Vec<_> = (0..p)
        .map(|_| {
            s.cmd(s.path(), Some(&s.store()))
                .args(["grep", &pattern])
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut k in kids {
        assert!(k.wait().unwrap().success());
    }
    let log = usage(&s);
    assert_eq!(log.len(), p + 1);
    assert!(log[1..].iter().all(|u| u["args"][0] == pattern.as_str()));
}

#[test]
fn an_unwritable_usage_log_fails_nothing() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    fs::create_dir(s.store().join("usage.jsonl")).unwrap();
    let r = s.ai(&["note", "still saved"]);
    assert!(r.status.success() && text(&r.stderr).is_empty());
    let r = s.ai(&["view"]);
    assert!(r.status.success() && text(&r.stdout).contains("still saved"));
}

#[test]
fn a_closed_pipe_is_quiet() {
    let s = Sandbox::new();
    s.ai(&["init"]);
    let seed = s.path().join("seed.txt");
    let lines: String = (0..5000)
        .map(|i| format!("2020-01-01 memory number {i} padded to make the output long enough\n"))
        .collect();
    fs::write(&seed, lines).unwrap();
    s.ai(&["import", seed.to_str().unwrap()]);
    let mut child = s
        .cmd(s.path(), Some(&s.store()))
        .args(["grep", "memory"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let r = child.wait_with_output().unwrap();
    assert!(r.status.success(), "{:?}", r.status);
    assert_eq!(text(&r.stderr), "");
}

/// A fake `claude`: answers every stream-json message with a fixed result.
const FAKE_CLAUDE: &str = "while read -r line; do
  printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"a fake summary\",\"stop_reason\":\"end_turn\",\"session_id\":\"fake-sess\"}'
done
";

#[test]
fn a_note_is_summarised_in_the_background() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    let path = s.fake_claude(FAKE_CLAUDE);
    for i in 0..2 {
        let r = s
            .cmd(s.path(), Some(&s.store()))
            .args(["note", &format!("note {i} {}", "z".repeat(250))])
            .env("AI_MEMORY_SUMMARISE", "1")
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(r.status.success(), "{}", text(&r.stderr));
        assert_eq!(text(&r.stdout), format!("Saved as {i}.\n"));
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let st = Store::open(&s.store()).unwrap();
        if let Some(n) = st.snapshot().unwrap().node(Coord::new(1, 0)).unwrap() {
            assert_eq!(
                (
                    n.text.as_str(),
                    n.who.agent.as_str(),
                    n.who.model.as_str(),
                    n.who.session.as_str(),
                    n.origin.as_str()
                ),
                (
                    "a fake summary",
                    "claude-code",
                    "sonnet",
                    "fake-sess",
                    st.origin()
                )
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no summariser: {}",
            fs::read_to_string(s.store().join("summarise.log")).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(50));
    }
    let r = s
        .cmd(s.path(), Some(&s.store()))
        .arg("build")
        .env("PATH", &path)
        .output()
        .unwrap();
    let out = text(&r.stdout);
    assert!(
        out == "Nothing to build.\n" || out.starts_with("A compactor is already running"),
        "{out}"
    );
}

const V1: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/memory-v1.bundle"
);
const V1_GOLDEN: &str = include_str!("fixtures/memory-v1.golden");

impl Sandbox {
    /// The format-1 fixture, written by ai 0.7.0, as this sandbox's store; its head.
    fn v1_store(&self) -> String {
        let d = self.store();
        self.git(self.path(), &["init", "-q", "--bare", d.to_str().unwrap()]);
        self.store_git(&["fetch", "-q", V1, &format!("{REF}:{REF}")]);
        self.store_git(&["rev-parse", REF])
    }

    /// `git` on the store, fed `input`.
    fn store_git_in(&self, args: &[&str], input: &str) -> String {
        let mut c = Command::new("git");
        self.isolate(&mut c);
        let mut kid = c
            .arg(format!("--git-dir={}", self.store().display()))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut kid.stdin.take().unwrap(), input.as_bytes()).unwrap();
        text(&kid.wait_with_output().unwrap().stdout)
            .trim_end()
            .to_string()
    }

    /// The commits on the memory ref, newest first.
    fn commits(&self) -> Vec<String> {
        let log = self.store_git(&["log", "--format=%H %s", REF]);
        log.lines().map(String::from).collect()
    }
}

/// The golden commands and their output, in order.
fn v1_golden() -> Vec<(Vec<&'static str>, String)> {
    let mut out: Vec<(Vec<&str>, String)> = Vec::new();
    for line in V1_GOLDEN.split_inclusive('\n') {
        match line.strip_prefix("$ ") {
            Some(cmd) => out.push((cmd.trim_end().split(' ').collect(), String::new())),
            None => out.last_mut().unwrap().1 += line,
        }
    }
    out
}

fn upgraded(head: &str) -> String {
    format!("Upgraded memory to format 2; previous state is commit {head}.\n")
}

#[test]
fn a_format_1_store_upgrades_once_and_reads_as_before() {
    let s = Sandbox::new();
    let old = s.v1_store();
    let golden = v1_golden();
    let (args, want) = &golden[0];
    let r = s.ai(args);
    assert_eq!(text(&r.stderr), upgraded(&old));
    assert_eq!((r.status.success(), text(&r.stdout)), (true, want.clone()));
    let commits = s.commits();
    assert_eq!(commits.len(), 16);
    let new = commits[0].split_once(' ').unwrap();
    assert_eq!(new.1, "upgrade format 1 -> 2");
    assert_eq!(s.store_git(&["rev-parse", &format!("{REF}^")]), old);
    assert_eq!(s.store_git(&["show", &format!("{REF}:format")]), "2");

    for (args, want) in &golden[1..] {
        let r = s.ai(args);
        assert!(
            r.status.success() && text(&r.stderr).is_empty(),
            "{args:?}: {}",
            text(&r.stderr)
        );
        assert_eq!(text(&r.stdout), *want, "{args:?}");
    }
    assert_eq!(
        s.commits()[0],
        commits[0],
        "a reopened store is not upgraded again"
    );

    let paths = s.store_git(&["ls-tree", "-r", "--name-only", &old]);
    assert_eq!(
        s.store_git(&["ls-tree", "-r", "--name-only", REF]),
        format!("format\n{paths}")
    );
    let blob = |rev: &str, p: &str| s.store_git(&["cat-file", "blob", &format!("{rev}:{p}")]);
    let ms = s.memories();
    let mut nodes = 0;
    for p in paths.lines() {
        let (was, is) = (blob(&old, p), blob(REF, p));
        if let Some(n) = p.strip_prefix("log/") {
            let i = usize::from_str_radix(&n.replace('/', ""), 16).unwrap();
            assert_eq!(ms[i].text, was.split_once('\n').unwrap().1, "message {i}");
            assert!(is.ends_with(&format!("\n\n{}", ms[i].text)));
            continue;
        }
        let (cols, lines) = is.split_once('\n').unwrap();
        assert_eq!(cols, "ts origin agent model session text");
        let text = |l: &str| l.splitn(6, ' ').nth(5).map(String::from);
        let (was, is): (Vec<_>, Vec<_>) = (
            was.lines().map(text).collect(),
            lines.lines().map(text).collect(),
        );
        assert_eq!(was, is, "{p}");
        nodes += was.len();
    }
    assert_eq!((ms.len(), nodes), (16, 31));
}

#[test]
fn concurrent_opens_upgrade_once() {
    let s = Sandbox::new();
    let old = s.v1_store();
    let want = v1_golden().remove(0).1;
    let kids: Vec<_> = (0..8)
        .map(|_| {
            s.cmd(s.path(), Some(&s.store()))
                .arg("view")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut told = 0;
    for k in kids {
        let r = k.wait_with_output().unwrap();
        assert_eq!((r.status.success(), text(&r.stdout)), (true, want.clone()));
        let err = text(&r.stderr);
        told += err.matches(&upgraded(&old)).count();
        let rest = err.replace(&upgraded(&old), "");
        assert!(rest.lines().all(|l| l.starts_with("Waiting for")), "{err}");
    }
    assert_eq!(told, 1);
    let ups = s
        .commits()
        .iter()
        .filter(|c| c.ends_with(" upgrade format 1 -> 2"))
        .count();
    assert_eq!(ups, 1);
}

#[test]
fn a_newer_format_is_refused_before_any_write() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    assert!(s.ai(&["note", "written by this ai"]).status.success());
    let tree = s.store_git(&["ls-tree", REF]);
    let blob = s.store_git_in(&["hash-object", "-w", "--stdin"], "3\n");
    let entries: String = tree
        .lines()
        .map(|l| match l.ends_with("\tformat") {
            true => format!("100644 blob {blob}\tformat\n"),
            false => format!("{l}\n"),
        })
        .collect();
    let tree = s.store_git_in(&["mktree"], &entries);
    let commit = s.store_git(&["commit-tree", "-p", REF, "-m", "from a newer ai", &tree]);
    s.store_git(&["update-ref", REF, &commit]);
    for args in [
        &["note", "x"][..],
        &["view"],
        &["build"],
        &["show", "0"],
        &["init"],
    ] {
        let r = s.ai(args);
        assert_eq!(
            (r.status.success(), text(&r.stderr).as_str()),
            (
                false,
                "This memory is format 3; this ai reads up to 2. Upgrade ai.\n"
            ),
            "{args:?}"
        );
    }
    assert_eq!(s.store_git(&["rev-parse", REF]), commit);
}

#[test]
fn the_root_is_the_default_branchs_first_commit() {
    let s = Sandbox::new();
    assert!(s.ai(&["init"]).status.success());
    let work = s.path().join("work");
    fs::create_dir(&work).unwrap();
    s.git(&work, &["init", "-q", "-b", "main"]);
    s.git(&work, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let root = s.git(&work, &["rev-parse", "HEAD"]);
    s.git(&work, &["commit", "-q", "--allow-empty", "-m", "second"]);
    s.git(&work, &["checkout", "-q", "--orphan", "side"]);
    s.git(&work, &["commit", "-q", "--allow-empty", "-m", "unrelated"]);
    let r = s
        .cmd(&work, Some(&s.store()))
        .args(["note", "on a side branch"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    assert_eq!(s.memories()[0].place.root, root);
    assert_eq!(s.memories()[0].place.branch, "side");
}

/// A fake `claude` writing summaries: a scope line, a blank line, a gist.
const FAKE_SUMMARIES: &str = "while read -r line; do
  printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"a fake scope\\n\\na fake gist\",\"stop_reason\":\"end_turn\"}'
done
";

#[test]
fn a_note_from_stdin_is_summarised_in_the_background() {
    let s = Sandbox::new();
    let path = s.fake_claude(FAKE_SUMMARIES);
    let mut c = Command::new(env!("CARGO_BIN_EXE_ai"));
    s.isolate(&mut c);
    let mut child = c
        .current_dir(s.path())
        .args([
            "notes",
            "add",
            "physics.qm",
            "--title",
            "Decoherence explains collapse",
        ])
        .args(["--confidence", "verified"])
        .env("AI_NOTES_SUMMARISE", "1")
        .env("PATH", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Decoherence explains apparent collapse.\n")
        .unwrap();
    let r = child.wait_with_output().unwrap();
    assert!(r.status.success(), "{}", text(&r.stderr));
    assert!(
        text(&r.stdout).starts_with("Saved as "),
        "{}",
        text(&r.stdout)
    );
    let notes = s.path().join("home/.ai/notes");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let log = s.git(&notes, &["log", "--format=%s"]);
        if log == "summary physics\nsummary physics.qm\nadd physics.qm" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{log}\nsummarise.log: {}",
            fs::read_to_string(notes.join(".git/summarise.log")).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(50));
    }
    let sum = fs::read_to_string(notes.join("physics/qm/_summary.md")).unwrap();
    assert!(sum.ends_with("---\na fake scope\n\na fake gist\n"), "{sum}");
    assert_eq!(s.git(&notes, &["status", "--porcelain"]), "");
}
