use std::{
    fs::{self, File},
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    process::{Command, Output},
    ptr, thread,
    time::{Duration, Instant},
};

use ai_tools::{
    agent::{Key, LineKind, NOT_ATTACHED, Store, Tree, claude, turns},
    memory::Who,
    project::Project,
};
use regex::Regex;
use serde_json::{Value, json};
use tempfile::TempDir;

const ISOLATE: [&str; 6] = [
    "AI_MEMORY_DIR",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDECODE",
    "GIT_DIR",
    "GIT_AUTHOR_NAME",
    "GIT_COMMITTER_NAME",
];

/// A project, a home whose `ai.toml` switches the background summariser off, a Claude Code
/// config dir and a fake `claude`, in a temporary directory.
struct Sandbox {
    _tmp: TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for d in ["home/.ai", "claude", "out", "bin"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(
            root.join("home/.ai/ai.toml"),
            "[chat]\nbackground = false\n",
        )
        .unwrap();
        let s = Sandbox { _tmp: tmp, root };
        s.init(&s.repo());
        s.fake_claude();
        s
    }

    /// A project at `dir`.
    fn init(&self, dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        Project::init(dir).unwrap();
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// Where Claude Code keeps the sessions started in `dir`.
    fn project(&self, dir: &Path) -> PathBuf {
        self.root
            .join("claude/projects")
            .join(claude::project_dir(dir))
    }

    /// Writes the transcript of `session` as Claude Code would, for a session started in the
    /// repository.
    fn transcript(&self, session: &str, text: &str) -> PathBuf {
        self.transcript_in(&self.repo(), session, text)
    }

    /// Writes the transcript of `session`, started in `dir`.
    fn transcript_in(&self, dir: &Path, session: &str, text: &str) -> PathBuf {
        let project = self.project(dir);
        fs::create_dir_all(&project).unwrap();
        let p = project.join(format!("{session}.jsonl"));
        fs::write(&p, text).unwrap();
        p
    }

    /// A `claude` that records its arguments, one per line, in `out/argv`, and its working
    /// directory in `out/cwd`.
    fn fake_claude(&self) {
        let path = std::env::var("PATH").unwrap();
        let bash = std::env::split_paths(&path)
            .map(|d| d.join("bash"))
            .find(|p| p.is_file())
            .expect("bash on PATH");
        let src = self.root.join("claude.sh");
        let out = self.root.join("out");
        let body = format!(
            "#!{}\nprintf '%s\\n' \"$@\" > {}\npwd > {}\n",
            bash.display(),
            out.join("argv").display(),
            out.join("cwd").display()
        );
        fs::write(&src, body).unwrap();
        let dst = self.root.join("bin/claude");
        assert!(
            Command::new("cp")
                .arg(&src)
                .arg(&dst)
                .status()
                .unwrap()
                .success()
        );
        fs::set_permissions(&dst, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }

    /// A `claude` that also answers as the compactor (`-p`), by the first message it gets: one
    /// holding `BAD` fails, one holding `LONG` gets a 600-byte line each time, any other gets
    /// a short line after `delay` seconds. Started as Claude Code, it records as
    /// [`Sandbox::fake_claude`].
    fn fake_compactor(&self, delay: &str) {
        let path = std::env::var("PATH").unwrap();
        let bash = std::env::split_paths(&path)
            .map(|d| d.join("bash"))
            .find(|p| p.is_file())
            .expect("bash on PATH");
        let out = self.root.join("out");
        let body = format!(
            r#"#!{bash}
if [ "$1" != "-p" ]; then
  printf '%s\n' "$@" > {argv}
  pwd > {cwd}
  exit 0
fi
long=$(printf '%0600d' 0)
mode=
while IFS= read -r l; do
  if [ -z "$mode" ]; then
    case "$l" in
      *BAD*) mode=bad ;;
      *LONG*) mode=long ;;
      *) mode=good ;;
    esac
  fi
  case $mode in
    bad) printf '%s\n' '{{"type":"result","is_error":true,"result":"overloaded"}}' ;;
    long) printf '{{"type":"result","result":"%s"}}\n' "$long" ;;
    good) sleep {delay}; printf '%s\n' '{{"type":"result","result":"a line"}}' ;;
  esac
done
"#,
            bash = bash.display(),
            argv = out.join("argv").display(),
            cwd = out.join("cwd").display(),
        );
        let dst = self.root.join("bin/claude");
        fs::remove_file(&dst).unwrap();
        fs::write(&dst, body).unwrap();
        fs::set_permissions(&dst, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }

    /// Whether thread `name` holds line `level@last`.
    fn has_line(&self, name: &str, level: u32, last: u64) -> bool {
        let t = self.store().thread(name).unwrap();
        t.snapshot()
            .unwrap()
            .line(Key { level, last })
            .unwrap()
            .is_some()
    }

    /// The arguments the fake `claude` was last run with.
    fn argv(&self) -> Vec<String> {
        let p = self.root.join("out/argv");
        let argv = fs::read_to_string(&p)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        fs::remove_file(p).unwrap();
        argv
    }

    /// The directory the fake `claude` was last run in.
    fn claude_dir(&self) -> PathBuf {
        let p = self.root.join("out/cwd");
        let dir = fs::read_to_string(&p).unwrap().trim_end().into();
        fs::remove_file(p).unwrap();
        dir
    }

    /// `ai` run in `dir`.
    fn bin(&self, dir: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ai"));
        for k in ISOLATE {
            c.env_remove(k);
        }
        let path = std::env::var("PATH").unwrap();
        c.current_dir(dir)
            .env("HOME", self.root.join("home"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("AI_MEMORY_SUMMARISE", "0")
            .env(
                "PATH",
                format!("{}:{path}", self.root.join("bin").display()),
            );
        c
    }

    fn cmd(&self, dir: &Path) -> Command {
        let mut c = self.bin(dir);
        c.arg("agent");
        c
    }

    fn ai(&self, args: &[&str]) -> Output {
        self.cmd(&self.repo()).args(args).output().unwrap()
    }

    /// `ai args` run in the repository, inside `session` if given.
    fn top(&self, session: Option<&str>, args: &[&str]) -> Output {
        let mut c = self.bin(&self.repo());
        if let Some(id) = session {
            c.env("CLAUDE_CODE_SESSION_ID", id);
        }
        c.args(args).output().unwrap()
    }

    /// `ai agent args` run outside any repository.
    fn outside(&self, args: &[&str]) -> Output {
        self.cmd(&self.root.join("out"))
            .args(args)
            .output()
            .unwrap()
    }

    /// `ai agent args` run inside session `session`, as its Bash tool runs it.
    fn ai_in(&self, session: &str, args: &[&str]) -> Output {
        self.ai_at(&self.repo(), session, args)
    }

    /// `ai agent args` run in `dir`, inside session `session`.
    fn ai_at(&self, dir: &Path, session: &str, args: &[&str]) -> Output {
        self.cmd(dir)
            .env("CLAUDE_CODE_SESSION_ID", session)
            .args(args)
            .output()
            .unwrap()
    }

    fn store(&self) -> Store {
        Store::find(&self.repo()).unwrap().unwrap()
    }

    /// Writes line `level@last` of thread `name`.
    fn line(&self, name: &str, level: u32, last: u64, text: &str) {
        let kind = match level {
            0 => LineKind::Chunk,
            1 => LineKind::Turn,
            _ => LineKind::Node,
        };
        let t = self.store().thread(name).unwrap();
        t.put_line(Key { level, last }, kind, &Who::default(), text)
            .unwrap();
    }

    /// `ai agent args` in the repository on a terminal, `keys` typed once it reads a key: its
    /// exit code and everything the terminal showed.
    fn on_terminal(&self, args: &[&str], keys: &[u8]) -> (i32, String) {
        let (mut master, slave) = pty();
        let mut c = self.cmd(&self.repo());
        c.args(args)
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap());
        let mut child = c.spawn().unwrap();
        drop(c);
        let deadline = Instant::now() + Duration::from_secs(10);
        while canonical(&slave) {
            assert!(child.try_wait().unwrap().is_none(), "it ended unasked");
            assert!(Instant::now() < deadline, "it never read a key");
            thread::sleep(Duration::from_millis(10));
        }
        drop(slave);
        master.write_all(keys).unwrap();
        let mut shown = Vec::new();
        let _ = master.read_to_end(&mut shown);
        let code = child.wait().unwrap().code().unwrap();
        (code, String::from_utf8_lossy(&shown).into())
    }
}

/// A pseudo-terminal of 24 rows by 80 columns: its master and slave ends.
fn pty() -> (File, File) {
    let (mut master, mut slave) = (0, 0);
    let size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let r = unsafe { libc::openpty(&mut master, &mut slave, ptr::null_mut(), ptr::null(), &size) };
    assert_eq!(r, 0, "openpty: {}", io::Error::last_os_error());
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}

/// Whether terminal `t` reads lines, not keys: the picker has not asked yet.
fn canonical(t: &File) -> bool {
    let mut tio = MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(t.as_raw_fd(), tio.as_mut_ptr()) },
        0
    );
    unsafe { tio.assume_init() }.c_lflag & libc::ICANON != 0
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

/// `git args` in `dir`, which must succeed: its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    stdout(&o)
}

fn stderr(o: &Output) -> String {
    String::from_utf8(o.stderr.clone()).unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

/// Exit 0 with this stdout.
fn ok(o: Output, want: &str) {
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""), "{}", stdout(&o));
    assert_eq!(stdout(&o), want);
}

/// Exit 1 with this message.
fn fails(o: Output, err: &str) {
    assert_eq!(
        (code(&o), stdout(&o).as_str(), stderr(&o).as_str()),
        (1, "", err)
    );
}

/// Exit 3, nothing printed.
fn unattached(o: Output) {
    assert_eq!(code(&o), i32::from(NOT_ATTACHED), "{}", stderr(&o));
    assert_eq!((stdout(&o).as_str(), stderr(&o).as_str()), ("", ""));
}

fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude");
    fs::read_to_string(p.join(name)).unwrap()
}

/// A transcript of `n` turns: the user's `ask(k)` and a reply that does not end the turn, so
/// it is the turn's work and the last turn stays open.
fn talk(n: usize, ask: impl Fn(usize) -> String) -> String {
    let mut out = String::new();
    let mut parent = Value::Null;
    for k in 0..n {
        let (u, a) = (format!("u{k}"), format!("a{k}"));
        let ts = format!("2026-10-14T10:{:02}:{:02}.000Z", k / 60, k % 60);
        let user = json!({
            "type": "user", "uuid": u, "parentUuid": parent, "timestamp": ts,
            "origin": {"kind": "human"},
            "message": {"role": "user", "content": ask(k)},
        });
        let reply = json!({
            "type": "assistant", "uuid": a, "parentUuid": u, "timestamp": ts,
            "message": {"model": "claude-opus-5-5", "role": "assistant",
                "content": [{"type": "text", "text": format!("reply {k}")}]},
        });
        out += &format!("{user}\n{reply}\n");
        parent = json!(a);
    }
    out
}

/// `text` with every entry recording `dir` as the directory it ran in.
fn ran_in(text: &str, dir: &Path) -> String {
    text.lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            v["cwd"] = json!(dir);
            format!("{v}\n")
        })
        .collect()
}

/// The sequence number of the last item of each turn on the newest branch of `name`.
fn turn_ends(s: &Sandbox, name: &str) -> Vec<u64> {
    let t = s.store().thread(name).unwrap();
    let tree: Tree = t.snapshot().unwrap().tree().unwrap();
    let tip = *tree.tips().last().unwrap();
    let branch = tree.branch(tip);
    turns(&tree.path(tip))
        .iter()
        .map(|x| branch[x.span.end - 1] as u64)
        .collect()
}

/// The lines of one section of a view.
fn section<'v>(view: &'v str, title: &str) -> Vec<&'v str> {
    let Some((_, rest)) = view.split_once(&format!("\n## {title}\n")) else {
        return Vec::new();
    };
    rest.split("\n\n## ").next().unwrap().lines().collect()
}

#[test]
fn plumbing_for_an_unattached_session_exits_3_silently() {
    let s = Sandbox::new();
    let flags = ["--harness", "claude", "--session", "s-1"];
    for cmd in [&["sync-session"][..], &["compact-session"]] {
        unattached(s.ai(&[cmd, &flags[..]].concat()));
        unattached(s.ai_in("s-1", cmd));
        unattached(s.ai(cmd));
    }
    unattached(s.outside(&["sync-session", "--session", "s-1"]));
}

#[test]
fn porcelain_without_a_thread_says_what_to_do() {
    let s = Sandbox::new();
    let attach = "This session is not attached to an agent, and this project has none. Create one, then attach this session: ai agent new <name>; ai agent attach <name>\n";
    let start = "No agents in this project. Create one: ai agent new <name>\n";
    for cmd in [&["view"][..], &["zoom", "0"], &["grep", "x"], &["build"]] {
        fails(s.ai_in("s-1", cmd), attach);
        fails(s.ai(cmd), start);
    }
    fails(
        s.ai_in("s-1", &["detach"]),
        "This session is not attached to an agent.\n",
    );
    fails(
        s.ai(&["view", "dev"]),
        "No agent dev in this project. Create it: ai agent new dev\n",
    );
    fails(
        s.outside(&["view", "dev"]),
        "Not in a project: no ai.toml here or above. Run `ai init` in the project's top directory.\n",
    );
}

#[test]
fn a_thread_is_named_else_attached_else_the_only_one_else_listed() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(2, |k| format!("question {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let dev = stdout(&s.ai(&["view", "dev"]));
    assert!(dev.starts_with("# Thread `dev`\n"), "{dev}");
    ok(s.ai(&["view"]), &dev);
    ok(s.ai_in("s-2", &["view"]), &dev);
    let zoom = stdout(&s.ai(&["zoom", "-a", "dev", "0"]));
    ok(s.ai_in("s-2", &["zoom", "0"]), &zoom);

    s.transcript("s-3", &talk(3, |k| format!("other {k}")));
    ok(
        s.ai_in("s-3", &["new", "other"]),
        "Created agent other. Start it: ai agent resume other\n",
    );
    ok(s.ai_in("s-3", &["attach", "other"]), "Attached to other.\n");
    let rows = "This project's agents, newest first:\n  other  3 turns  2026-10-14 10:00\n  dev  2 turns  2026-10-14 10:00\n";
    fails(
        s.ai(&["view"]),
        &format!("Name an agent: ai agent view <name>. {rows}"),
    );
    fails(
        s.ai_in("s-2", &["zoom", "0"]),
        &format!(
            "This session is not attached to an agent. Attach it with `ai agent attach <name>`, or name one: ai agent zoom -a <name> <id>. {rows}"
        ),
    );
    ok(s.ai_in("s-1", &["view"]), &dev);
    ok(s.ai_in("s-1", &["zoom", "0"]), &zoom);
    ok(
        s.ai_in("s-1", &["view", "other"]),
        &stdout(&s.ai(&["view", "other"])),
    );
}

#[test]
fn on_a_terminal_a_picker_chooses_the_thread() {
    let s = Sandbox::new();
    for (session, name, n) in [("s-1", "dev", 2), ("s-2", "other", 3)] {
        s.transcript(session, &talk(n, |k| format!("{name} {k}")));
        assert_eq!(code(&s.ai(&["new", name])), 0);
        assert_eq!(code(&s.ai_in(session, &["attach", name])), 0);
    }
    let (code, shown) = s.on_terminal(&["view"], b"\x1b");
    assert_eq!(code, 1, "{shown}");
    assert!(shown.contains("No agent chosen."), "{shown}");
    assert!(!shown.contains("# Thread"), "{shown}");

    let (code, shown) = s.on_terminal(&["view"], b"\r");
    assert_eq!(code, 0, "{shown}");
    assert!(
        shown.contains("# Thread `other`"),
        "Enter takes the newest, listed first: {shown}"
    );
}

#[test]
fn plumbing_is_hidden_from_help() {
    let s = Sandbox::new();
    let help = stdout(&s.ai(&["--help"]));
    for cmd in [
        "resume", "list", "view", "attach", "detach", "zoom", "build",
    ] {
        assert!(help.contains(&format!("\n  {cmd} ")), "{cmd}: {help}");
    }
    for cmd in [
        "sync-session",
        "compact-session",
        "end-session",
        "nap",
        "claude",
    ] {
        assert!(!help.contains(cmd), "{cmd}: {help}");
    }
    for cmd in ["list", "view", "attach", "detach", "zoom", "grep", "build"] {
        let help = stdout(&s.ai(&[cmd, "--help"]));
        for flag in ["--harness", "--session", "--transcript", "--part", "--at"] {
            assert!(!help.contains(flag), "{cmd} {flag}: {help}");
        }
    }
    let help = stdout(&s.top(None, &["view", "--help"]));
    for flag in ["--harness", "--session", "--transcript"] {
        assert!(!help.contains(flag), "ai view {flag}: {help}");
    }
}

#[test]
fn attach_sync_and_detach() {
    let s = Sandbox::new();
    let all = fixture("interject.jsonl");
    let half: String = all.split_inclusive('\n').take(8).collect();
    s.transcript("s-1", &half);
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let t = s.store().thread("dev").unwrap();
    let n = t.snapshot().unwrap().len().unwrap();
    assert!(n > 0, "the transcript is found from the session id");
    let path = s.transcript("s-1", &all);
    ok(
        s.ai(&[
            "sync-session",
            "--session",
            "s-1",
            "--transcript",
            path.to_str().unwrap(),
        ]),
        "",
    );
    assert!(t.snapshot().unwrap().len().unwrap() > n);
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(
        s.ai(&["new", "other"]),
        "Created agent other. Start it: ai agent resume other\n",
    );
    fails(
        s.ai(&["attach", "other", "--harness", "claude", "--session", "s-1"]),
        "Session claude s-1 is attached to dev. Run ai agent detach --harness claude --session s-1 before attaching to other.\n",
    );
    ok(s.ai_in("s-1", &["detach"]), "Detached from dev.\n");
    ok(s.ai_in("s-1", &["attach", "other"]), "Attached to other.\n");
    ok(s.ai_in("s-1", &["detach"]), "Detached from other.\n");
    fails(
        s.ai_in("s-1", &["detach"]),
        "This session is not attached to an agent.\n",
    );

    for cmd in ["attach", "detach"] {
        let args = [cmd, "dev"];
        fails(
            s.ai(&args[..usize::from(cmd == "attach") + 1]),
            "No session: run this inside an agent session, or pass --session.\n",
        );
    }
    let o = s.ai_in("s-1", &["attach", "a/b"]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("is not a thread name"));
    let o = s.outside(&["attach", "dev", "--session", "s-1"]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).starts_with("Not in a project"));
}

#[test]
fn compact_session_syncs_and_prints_the_view() {
    let s = Sandbox::new();
    let all = fixture("interject.jsonl");
    let first: String = all.split_inclusive('\n').take(14).collect();
    s.transcript("s-1", &first);
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    s.transcript("s-1", &all);
    let ends = turn_ends(&s, "dev");
    s.line("dev", 1, ends[0], "user asked to run the tests; two failed");
    let o = s.ai(&["compact-session", "--harness", "claude", "--session", "s-1"]);
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""));
    let view = stdout(&o);
    assert!(view.starts_with("# Thread `dev`\n\nThis is the thread `dev`"));
    assert!(view.contains("`ai agent zoom 8+4`"));
    let ends = turn_ends(&s, "dev");
    assert_eq!(
        ends.len(),
        4,
        "compact-session synced the rest of the transcript"
    );
    let list = section(&view, "Turns");
    assert_eq!(list[0], "0.0|user: Run the tests and fix what fails.");
    assert!(
        list[1].starts_with("0.1+3|ai: Running the tests. tool Bash: "),
        "compact-session built the recent turns' work lines, free, without ids: {list:#?}"
    );
    assert_eq!(list[2], "1.0|user: Skip the network tests.");
    assert!(list[3].starts_with("1.1+6|ai: Skipping the network tests. tool Bash: "));
    assert!(
        !view.contains("user asked to run the tests"),
        "a recent turn shows its work lines, not its turn line"
    );
    assert_eq!(
        list[list.len() - 2..],
        [
            "3.0|user: Show me the diff instead.",
            "3.1|ai: The diff changes the test filter only."
        ],
        "the newest turn, ended, shows every item"
    );
    assert_eq!(view.matches("\n## ").count(), 1, "one list: {view}");
}

#[test]
fn compact_session_fails_with_its_reason() {
    let s = Sandbox::new();
    let path = s.root.join("out/p-1.jsonl");
    let p = path.to_str().unwrap();
    let flags = ["--harness", "pi", "--session", "p-1", "--transcript", p];
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(
        s.ai(&[&["attach", "dev"][..], &flags].concat()),
        "Attached to dev.\n",
    );
    fs::write(&path, "{}\n").unwrap();
    fails(
        s.ai(&[&["compact-session"][..], &flags].concat()),
        "ai agent cannot read pi sessions.\n",
    );
}

#[test]
fn list_shows_this_repositorys_threads_newest_first_and_their_sessions() {
    let s = Sandbox::new();
    ok(
        s.ai(&["list"]),
        "No agents in this project. Create one: ai agent new <name>\n",
    );
    s.transcript("s-1", &fixture("rewind.jsonl"));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    s.transcript("s-2", &talk(100, |k| format!("question {k}")));
    ok(
        s.ai_in("s-2", &["new", "long-one"]),
        "Created agent long-one. Start it: ai agent resume long-one\n",
    );
    ok(
        s.ai_in("s-2", &["attach", "long-one"]),
        "Attached to long-one.\n",
    );
    s.transcript("s-10", &talk(1, |_| "later".into()));
    ok(s.ai_in("s-10", &["attach", "dev"]), "Attached to dev.\n");
    ok(s.ai_in("s-10", &["detach"]), "Detached from dev.\n");
    ok(
        s.ai_in("s-2", &["list"]),
        "long-one  100 turns  2026-10-14 10:01\n\
         \x20 claude  s-2  attached  2026-10-14 10:01  this session\n\
         dev  3 turns  2026-10-14 10:00  2 branches\n\
         \x20 claude  s-1   attached  2026-10-14 13:02\n\
         \x20 claude  s-10  detached  2026-10-14 10:00\n",
    );
}

#[test]
fn view_prints_a_thread_in_parts_from_one_state() {
    let s = Sandbox::new();
    let ask = |k| {
        format!(
            "question {k} {}",
            "q".repeat(if k == 99 { 4_000 } else { 200 })
        )
    };
    s.transcript("s-2", &talk(100, ask));
    ok(
        s.ai_in("s-2", &["new", "long-one"]),
        "Created agent long-one. Start it: ai agent resume long-one\n",
    );
    ok(
        s.ai_in("s-2", &["attach", "long-one"]),
        "Attached to long-one.\n",
    );
    let ends = turn_ends(&s, "long-one");
    let text = |k: usize| format!("{k} {}", "w".repeat(500));
    for (k, &end) in ends.iter().enumerate() {
        s.line("long-one", 1, end, &text(k));
    }
    // 99 closed turns fold to 64 lines: the 20 oldest in nodes of 4, the next 40 in pairs.
    for i in 0..35 {
        s.line("long-one", 2, ends[2 * i + 1], &text(i));
    }
    for i in 0..5 {
        s.line("long-one", 3, ends[4 * i + 3], &text(i));
    }
    let o = s.ai(&["compact-session", "--session", "s-2"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let view = stdout(&o);
    assert!(
        view.len() > 2 * 20_000 && view.len() < 3 * 19_000,
        "{}",
        view.len()
    );
    let first = stdout(&s.ai_in("s-2", &["view"]));
    assert_eq!(
        first,
        stdout(&s.ai(&["view", "long-one"])),
        "outside the session: the thread's newest branch, here the same"
    );
    let (body, footer) = first.trim_end().rsplit_once('\n').unwrap();
    let next = footer.strip_prefix("Part 1 of 3. Next: ai agent ").unwrap();
    assert!(next.starts_with("view long-one --part 2 --at "));
    let mut parts = body.to_string() + "\n";
    s.transcript("s-2", &talk(101, ask));
    ok(s.ai_in("s-2", &["sync-session"]), "");
    assert_ne!(
        stdout(&s.ai_in("s-2", &["view"])),
        first,
        "the thread moved on"
    );
    let args: Vec<&str> = next.split(' ').collect();
    let second = stdout(&s.ai(&args));
    let (body, footer) = second.trim_end().rsplit_once('\n').unwrap();
    parts += &(body.to_string() + "\n");
    let args: Vec<&str> = footer
        .split_once("Next: ai agent ")
        .unwrap()
        .1
        .split(' ')
        .collect();
    let third = stdout(&s.ai_in("s-2", &args));
    let (body, footer) = third.trim_end().rsplit_once('\n').unwrap();
    assert_eq!(footer, "Part 3 of 3.");
    parts += &(body.to_string() + "\n");
    assert_eq!(parts, view, "the parts come from one state");
    let o = s.ai_in("s-2", &["view", "--part", "2", "--at", "xyz"]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("'xyz' is not a thread state"));
}

#[test]
fn agent_alone_prints_the_view() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(2, |k| format!("question {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let view = stdout(&s.ai_in("s-1", &["view"]));
    assert!(view.contains("question 1"), "{view}");
    ok(s.ai_in("s-1", &[]), &view);
}

#[test]
fn view_syncs_its_session_and_writes_no_line() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(2, |k| format!("question {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let lines = |s: &Sandbox| {
        let t = s.store().thread("dev").unwrap();
        t.snapshot().unwrap().all_lines().unwrap().len()
    };
    s.transcript("s-1", &talk(4, |k| format!("question {k}")));

    let outside = stdout(&s.ai(&["view", "dev"]));
    assert_eq!(
        section(&outside, "Turns"),
        [
            "0.0|user: question 0",
            "0.1|(not summarised yet)",
            "1.0|user: question 1",
            "1.1|ai: reply 1",
        ],
        "outside the session, nothing is synced: {outside}"
    );

    let inside = stdout(&s.ai_in("s-1", &["view"]));
    assert_eq!(
        section(&inside, "Turns")[4..],
        [
            "2.0|user: question 2",
            "2.1|(not summarised yet)",
            "3.0|user: question 3",
            "3.1|ai: reply 3",
        ],
        "the session's view syncs it first: {inside}"
    );
    assert_eq!(lines(&s), 0, "view writes no line");

    let o = s.ai(&["compact-session", "--session", "s-1"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(
        lines(&s),
        3,
        "compact-session writes the lines its view needs: the recent turns' work lines"
    );
    let view = stdout(&o);
    assert!(
        !section(&view, "Turns")
            .iter()
            .any(|l| l.contains("not summarised yet")),
        "{view}"
    );
}

#[test]
fn ai_view_prints_the_whole_memory_then_a_thread() {
    let s = Sandbox::new();
    let memory = |args: &[&str]| s.top(None, &[&["memory"][..], args].concat());
    assert_eq!(code(&memory(&["init"])), 0);
    for k in 0..80 {
        let note = format!("note {k} {}", "n".repeat(260));
        assert_eq!(code(&memory(&["note", &note])), 0);
    }

    let alone = s.top(None, &["view", "--whole"]);
    assert_eq!((code(&alone), stderr(&alone).as_str()), (0, ""));
    let alone = stdout(&alone);
    assert!(alone.starts_with("# Memory\n\n"), "{alone}");
    assert!(alone.contains("\n0|note 0 n") && alone.contains("\n79|note 79 n"));
    assert!(!alone.contains("\nPart "), "{alone}");
    assert!(
        alone.ends_with(
            "\nYou are awake. Zoom the marked lines that touch your task before you act.\n"
        ),
        "with no thread, only the memory: {alone}"
    );
    assert_eq!(
        paged(&s, None, &["view"], "ai "),
        alone,
        "an agent reads it in parts"
    );

    s.transcript("s-1", &talk(2, |k| format!("question {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let both = stdout(&s.top(Some("s-1"), &["view", "--whole"]));
    let (mem, thread) = both.split_once("\n# Thread `dev`\n").expect(&both);
    assert_eq!(mem, alone);
    assert!(thread.contains("|user: question 1"), "{thread}");
    let first = stdout(&s.top(Some("s-1"), &["view"]));
    assert!(
        Regex::new(
            r"\nPart 1 of 2\. Next: ai view --part 2 --at [0-9a-f]{40}:dev:\d+\.\d+\.\d+\n$"
        )
        .unwrap()
        .is_match(&first),
        "{first}"
    );
    assert_eq!(paged(&s, Some("s-1"), &["view"], "ai "), both);
    assert_eq!(
        stdout(&s.top(None, &["view", "--whole"])),
        both,
        "outside a session, the repository's only thread"
    );
    assert_eq!(
        stdout(&s.top(Some("s-2"), &["view", "--whole"])),
        alone,
        "a session attached to none reads no thread unless named"
    );
    assert_eq!(
        stdout(&s.top(Some("s-2"), &["view", "dev", "--whole"])),
        both
    );

    s.transcript("s-3", &talk(3, |k| format!("other {k}")));
    ok(
        s.ai_in("s-3", &["new", "other"]),
        "Created agent other. Start it: ai agent resume other\n",
    );
    ok(s.ai_in("s-3", &["attach", "other"]), "Attached to other.\n");
    fails(
        s.top(None, &["view"]),
        "Name an agent: ai view <name>. This project's agents, newest first:\n  other  3 turns  2026-10-14 10:00\n  dev  2 turns  2026-10-14 10:00\n",
    );

    let usage = fs::read_to_string(s.root.join("home/.ai/memory/usage.jsonl")).unwrap();
    assert_eq!(
        usage
            .lines()
            .filter(|l| l.contains(r#""cmd":"view","args":["--at""#))
            .count(),
        10,
        "each ai view reads the memory once, after its thread is known: {usage}"
    );
}

/// The output of `args` run as `ai` in `session`, and then each part's next command: the parts
/// joined, without their footers.
fn paged(s: &Sandbox, session: Option<&str>, args: &[&str], prefix: &str) -> String {
    let mut out = String::new();
    let mut args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    loop {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let o = s.top(session, &argv);
        assert_eq!(code(&o), 0, "{}", stderr(&o));
        let o = stdout(&o);
        let Some((body, footer)) = o.trim_end().rsplit_once("\nPart ") else {
            return out + &o;
        };
        out += body;
        out.push('\n');
        match footer.split_once(&format!("Next: {prefix}")) {
            Some((_, next)) => args = next.split(' ').map(str::to_string).collect(),
            None => return out,
        }
    }
}

#[test]
fn zoom_opens_lines_and_items() {
    let s = Sandbox::new();
    let mut text = talk(4, |k| format!("question {k}"));
    let long = json!({
        "type": "user", "uuid": "u4", "parentUuid": "a3",
        "timestamp": "2026-10-14T11:00:00.000Z", "origin": {"kind": "human"},
        "message": {"role": "user", "content": "paste ".repeat(5_000)},
    });
    text += &format!("{long}\n");
    s.transcript("s-1", &text);
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    s.line("dev", 1, 1, "first");
    s.line("dev", 1, 3, "second");
    s.line("dev", 2, 3, "first two");
    ok(s.ai_in("s-1", &["zoom", "0+2"]), "0|first\n1|second\n");
    ok(s.ai(&["zoom", "-a", "dev", "0+2"]), "0|first\n1|second\n");
    ok(
        s.ai_in("s-1", &["zoom", "0+4"]),
        "0|first\n1|second\n2|(not summarised yet)\n3|(not summarised yet)\n",
    );
    ok(
        s.ai_in("s-1", &["zoom", "1+3"]),
        "1|second\n2+2|(not summarised yet)\n",
    );
    ok(
        s.ai_in("s-1", &["zoom", "2"]),
        "2.0|user: question 2\n2.1|ai: reply 2\n",
    );
    ok(
        s.ai_in("s-1", &["zoom", "2", "--tool-output"]),
        "2.0|user: question 2\n2.1|ai: reply 2\n",
    );
    ok(s.ai_in("s-1", &["zoom", "2.1"]), "2.1|ai: reply 2\n");
    let next = |args: &[&str], want: &str| -> String {
        let out = stdout(&s.ai_in("s-1", args));
        let (body, next) = out
            .split_once("\nPart 1 of 2. Next: ai agent ")
            .expect(&out);
        assert!(Regex::new(want).unwrap().is_match(next), "{next}");
        let rest = stdout(&s.ai(&next.split_whitespace().collect::<Vec<_>>()));
        format!(
            "{body}{}",
            rest.strip_suffix("Part 2 of 2.\n").expect(&rest)
        )
    };
    assert_eq!(
        next(
            &["zoom", "4.0"],
            r"^zoom -a dev 4\.0 --part 2 --at \d+\.\d+\.\d+\n$"
        ),
        format!("4.0|user: {}\n", "paste ".repeat(5_000).trim_end())
    );
    next(
        &["zoom", "4", "--tool-output"],
        r"^zoom -a dev 4 --part 2 --at \S+ --tool-output\n$",
    );
    for (id, err) in [
        (
            "x",
            "'x' is not an id. Ids look like 12 (a turn), 8+4 (turns 8 to 11) or 12.3 (an item).\n",
        ),
        ("0+8", "This branch has no 0+8.\n"),
        ("5", "This branch has no 5.\n"),
        ("2.2", "This branch has no 2.2.\n"),
    ] {
        fails(s.ai_in("s-1", &["zoom", id]), err);
    }
}

#[test]
fn grep_pages_the_newest_matches_first() {
    let s = Sandbox::new();
    s.transcript(
        "s-1",
        &talk(300, |k| format!("question {k} {}", "w".repeat(100))),
    );
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(
        s.ai_in("s-1", &["grep", "reply 7$"]),
        "7.1|ai: reply 7\n1 match.\n",
    );
    ok(
        s.ai(&["grep", "-a", "dev", "reply 7$"]),
        "7.1|ai: reply 7\n1 match.\n",
    );
    ok(s.ai_in("s-1", &["grep", "nothing"]), "No match.\n");
    let mut seen = Vec::new();
    let mut args = vec!["grep".to_string(), "Question".into(), "-i".into()];
    let mut k = 0;
    loop {
        k += 1;
        let out = stdout(&s.ai_in("s-1", &args.iter().map(String::as_str).collect::<Vec<_>>()));
        let (body, footer) = out.trim_end().rsplit_once('\n').unwrap();
        let page: Vec<&str> = body.lines().collect();
        assert!(body.len() < 20_000);
        seen.splice(
            0..0,
            page.iter()
                .map(|l| l.split('|').next().unwrap().to_string()),
        );
        let head = format!("Part {k} of ");
        assert!(footer.starts_with(&head), "{footer}");
        match footer.split_once(" Next: ai agent ") {
            Some((_, next)) => {
                let want = format!(r"^grep -a dev 'Question' -i --part {} --at \S+$", k + 1);
                assert!(Regex::new(&want).unwrap().is_match(next), "{next}");
                args = next
                    .split(' ')
                    .map(|a| a.trim_matches('\'').to_string())
                    .collect();
            }
            None => {
                assert_eq!(
                    footer,
                    format!(
                        "Part {k} of {k}, newest first: {} of 300 matches.",
                        page.len()
                    )
                );
                break;
            }
        }
    }
    assert!(k > 1);
    let want: Vec<String> = (0..300).map(|k| format!("{k}.0")).collect();
    assert_eq!(seen, want, "every match once, oldest first");
    fails(
        s.ai_in("s-1", &["grep", "Question", "-i", "--part", "99"]),
        &format!("No part 99: it has {k}.\n"),
    );
}

#[test]
fn grep_searches_each_agent_named() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(2, |k| format!("dev asks {k}")));
    s.transcript("s-2", &talk(2, |k| format!("ops asks {k}")));
    let long = |who: &'static str| move |k| format!("{who} says {k} {}", "w".repeat(100));
    s.transcript("s-3", &talk(150, long("one")));
    s.transcript("s-4", &talk(150, long("two")));
    for (session, name) in [
        ("s-1", "dev"),
        ("s-2", "ops"),
        ("s-3", "one"),
        ("s-4", "two"),
    ] {
        stdout(&s.ai_in(session, &["new", name]));
        ok(
            s.ai_in(session, &["attach", name]),
            &format!("Attached to {name}.\n"),
        );
    }
    ok(
        s.ai(&["grep", "-a", "ops", "-a", "dev", "asks 1"]),
        "ops 1.0|user: ops asks 1\ndev 1.0|user: dev asks 1\n2 matches.\n",
    );
    ok(
        s.ai(&["grep", "-a", "dev", "-a", "dev", "asks 1"]),
        "1.0|user: dev asks 1\n1 match.\n",
    );
    fails(
        s.ai(&["grep", "-a", "dev", "-a", "qa", "asks"]),
        "No agent qa in this project. Create it: ai agent new qa\n",
    );
    let mut args: Vec<String> = ["grep", "-a", "one", "-a", "two", "says"]
        .map(String::from)
        .into();
    let mut seen = Vec::new();
    let mut parts = 0;
    for k in 1.. {
        parts = k;
        let out = stdout(&s.ai(&args.iter().map(String::as_str).collect::<Vec<_>>()));
        let (body, footer) = out.trim_end().rsplit_once('\n').unwrap();
        seen.splice(
            0..0,
            body.lines()
                .map(|l| l.split('|').next().unwrap().to_string()),
        );
        let Some((_, next)) = footer.split_once(" Next: ai agent ") else {
            break;
        };
        let want = format!(r"^grep -a one -a two 'says' --part {} --at \S+,\S+$", k + 1);
        assert!(Regex::new(&want).unwrap().is_match(next), "{next}");
        args = next
            .split(' ')
            .map(|a| a.trim_matches('\'').to_string())
            .collect();
    }
    let want: Vec<String> = ["one", "two"]
        .iter()
        .flat_map(|a| (0..150).map(move |k| format!("{a} {k}.0")))
        .collect();
    assert!(parts > 1);
    assert_eq!(seen, want, "every match once, agent by agent");
    let at = s.ai(&["grep", "-a", "dev", "-a", "ops", "asks", "--at", "0.0.0"]);
    fails(
        at,
        "'0.0.0' does not name one state for each agent searched.\n",
    );
}

#[test]
fn resuming_a_new_agent_binds_a_new_session_and_starts_claude() {
    let s = Sandbox::new();
    fails(
        s.ai(&["resume", "dev"]),
        "No agent dev in this project. Create it: ai agent new dev\n",
    );
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai(&["resume", "dev", "--", "--model", "haiku"]), "");
    let argv = s.argv();
    assert_eq!(argv[0], "--session-id");
    assert_eq!(argv[2..], ["--model", "haiku"]);
    assert_eq!(s.claude_dir(), s.repo());
    let id = &argv[1];
    assert_eq!(id.len(), 36);
    let t = s.store().thread("dev").unwrap();
    let b = t
        .snapshot()
        .unwrap()
        .binding("claude", id)
        .unwrap()
        .unwrap();
    assert!(b.is_attached());
    let want = s.project(&s.repo()).join(format!("{id}.jsonl"));
    assert_eq!(b.transcript, Some(want.clone()));
    assert_eq!((b.graft, b.tip), (None, None));
    ok(s.ai(&["compact-session", "--session", id]), "");
    ok(s.ai_in(id, &["view"]), "Thread dev holds nothing yet.\n");

    fs::create_dir_all(want.parent().unwrap()).unwrap();
    fs::write(&want, fixture("plain.jsonl")).unwrap();
    unattached(s.ai_in("other", &["sync-session"]));
    ok(s.ai_in(id, &["sync-session"]), "");
    fs::remove_file(&want).unwrap();
    let o = s.ai(&["resume", "dev"]);
    assert_eq!(
        stderr(&o),
        format!(
            "Cannot resume dev: the transcript of session {id} is gone. Starting a new session.\n"
        )
    );
    let second = &s.argv()[1];
    assert_ne!(second, id);
    let b = t
        .snapshot()
        .unwrap()
        .binding("claude", second)
        .unwrap()
        .unwrap();
    assert_eq!(
        b.graft,
        Some(3),
        "a new session hangs on the thread's newest item"
    );
    let o = s.ai(&["compact-session", "--session", second]);
    let seed = stdout(&o);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(seed.starts_with("# Thread `dev`"), "{seed}");
    assert!(
        section(&seed, "Turns")
            .last()
            .is_some_and(|l| l.starts_with("1.1|")),
        "a new session's view ends at its graft, the previous session's ended turn: {seed}"
    );

    let o = s.ai(&["resume", "a b"]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("is not a thread name"));
}

#[test]
fn resume_continues_the_newest_session_in_its_directory_or_starts_one() {
    let s = Sandbox::new();
    fails(
        s.ai(&["resume", "dev"]),
        "No agent dev in this project. Create it: ai agent new dev\n",
    );
    s.transcript("s-1", &ran_in(&fixture("plain.jsonl"), &s.repo()));
    let newest = s.transcript("s-2", &ran_in(&talk(1, |_| "later".into()), &s.repo()));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(s.ai_in("s-2", &["attach", "dev"]), "Attached to dev.\n");
    ok(s.ai_in("s-2", &["detach"]), "Detached from dev.\n");

    ok(s.ai(&["resume", "dev", "--", "--model", "haiku"]), "");
    assert_eq!(s.argv(), ["--resume", "s-2", "--model", "haiku"]);
    assert_eq!(s.claude_dir(), s.repo());
    let t = s.store().thread("dev").unwrap();
    let b = t
        .snapshot()
        .unwrap()
        .binding("claude", "s-2")
        .unwrap()
        .unwrap();
    assert!(b.is_attached(), "resuming re-attaches it");

    let sub = s.repo().join("sub");
    fs::create_dir(&sub).unwrap();
    let o = s.cmd(&sub).args(["resume", "dev"]).output().unwrap();
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""));
    assert_eq!(s.argv(), ["--resume", "s-2"]);
    assert_eq!(
        s.claude_dir(),
        s.repo(),
        "resumed where the session ran, from a subdirectory"
    );

    let away = s.root.join("out");
    fs::write(&newest, ran_in(&talk(1, |_| "later".into()), &away)).unwrap();
    let o = s.ai(&["resume", "dev"]);
    assert_eq!(
        stderr(&o),
        format!(
            "Cannot resume dev: session s-2 ran in {}, outside this project. Starting a new session.\n",
            away.display()
        )
    );
    assert_eq!(s.argv()[0], "--session-id");

    fs::remove_file(newest).unwrap();
    let o = s.ai(&["resume", "dev", "--", "--model", "haiku"]);
    assert_eq!(
        stderr(&o),
        "Cannot resume dev: the transcript of session s-2 is gone. Starting a new session.\n"
    );
    let argv = s.argv();
    assert_eq!(argv[0], "--session-id");
    assert_eq!(argv[2..], ["--model", "haiku"]);
    let b = t
        .snapshot()
        .unwrap()
        .binding("claude", &argv[1])
        .unwrap()
        .unwrap();
    assert!(b.is_attached() && b.graft.is_some());
}

#[test]
fn ai_resume_is_ai_agent_resume() {
    let s = Sandbox::new();
    let o = s.top(None, &["resume", "dev"]);
    assert_eq!(code(&o), 1);
    assert_eq!(
        stderr(&o),
        "No agent dev in this project. Create it: ai agent new dev\n"
    );
    s.transcript("s-1", &ran_in(&fixture("plain.jsonl"), &s.repo()));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(s.ai_in("s-1", &["detach"]), "Detached from dev.\n");

    let o = s.top(None, &["resume", "dev", "--", "--model", "haiku"]);
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""));
    assert_eq!(s.argv(), ["--resume", "s-1", "--model", "haiku"]);
    assert_eq!(s.claude_dir(), s.repo());
}

#[test]
fn an_agent_runs_in_its_worktree_with_its_instructions() {
    let s = Sandbox::new();
    let toml = s.repo().join("ai.toml");
    let project = fs::read_to_string(&toml).unwrap();
    fs::write(
        &toml,
        format!(
            "{project}\n[agents.dev]\ninstructions = \"agents/dev.md\"\nworktree = \"../dev-wt\"\n"
        ),
    )
    .unwrap();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );

    let given = s.repo().join("../dev-wt");
    fails(
        s.ai(&["resume", "dev"]),
        &format!(
            "Agent dev's worktree {0} does not exist. Create it: git worktree add {0}\n",
            given.display()
        ),
    );
    let wt = s.root.join("dev-wt");
    fs::create_dir(&wt).unwrap();
    let id = Project::find(&s.repo()).unwrap().unwrap().id;
    let unshared = format!(
        "Agent dev's worktree {} does not share this project's agents: make it a git worktree \
         of {}, holding an ai.toml with the id {id}.\n",
        wt.display(),
        s.repo().display()
    );
    fails(s.ai(&["resume", "dev"]), &unshared);
    fs::write(wt.join("ai.toml"), &project).unwrap();
    fails(s.ai(&["resume", "dev"]), &unshared);
    fs::remove_dir_all(&wt).unwrap();
    git(&s.repo(), &["init", "-q"]);
    git(&s.repo(), &["add", "."]);
    git(&s.repo(), &["commit", "-qm", "init"]);
    git(&s.repo(), &["worktree", "add", "-q", "../dev-wt"]);
    fails(
        s.ai(&["resume", "dev"]),
        &format!(
            "Cannot read agent dev's instructions: {}: No such file or directory.\n",
            s.repo().join("agents/dev.md").display()
        ),
    );
    assert!(!s.root.join("out/argv").exists(), "nothing started");

    fs::create_dir(s.repo().join("agents")).unwrap();
    fs::write(s.repo().join("agents/dev.md"), "Keep it short.").unwrap();
    let o = s.ai(&["resume", "dev", "--harness", "pi"]);
    assert_eq!(code(&o), 2, "only claude is a harness yet");
    ok(
        s.ai(&[
            "resume",
            "dev",
            "--harness",
            "claude",
            "--",
            "--model",
            "haiku",
        ]),
        "",
    );
    let argv = s.argv();
    let session = argv[1].clone();
    assert_eq!(
        argv,
        [
            "--session-id",
            &session,
            "--append-system-prompt",
            "Keep it short.",
            "--model",
            "haiku"
        ]
    );
    assert_eq!(s.claude_dir(), wt);
    let t = s.store().thread("dev").unwrap();
    let b = t
        .snapshot()
        .unwrap()
        .binding("claude", &session)
        .unwrap()
        .unwrap();
    let transcript = s.project(&wt).join(format!("{session}.jsonl"));
    assert_eq!(
        b.transcript,
        Some(transcript.clone()),
        "Claude Code keeps the session under the worktree"
    );
    ok(
        s.ai_at(&wt, &session, &["view"]),
        "Thread dev holds nothing yet.\n",
    );

    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, ran_in(&fixture("plain.jsonl"), &wt)).unwrap();
    ok(s.ai_at(&wt, &session, &["sync-session"]), "");
    ok(s.ai(&["resume", "dev"]), "");
    assert_eq!(
        s.argv(),
        [
            "--resume",
            &session,
            "--append-system-prompt",
            "Keep it short."
        ]
    );
    assert_eq!(s.claude_dir(), wt);

    fs::write(
        s.repo().join("ai.local.toml"),
        "[agents.dev]\ninstructions = \"agents/mine.md\"\n",
    )
    .unwrap();
    fs::write(s.repo().join("agents/mine.md"), "Say why.").unwrap();
    ok(s.ai(&["resume", "dev"]), "");
    let argv = s.argv();
    assert_eq!(argv[..3], ["--resume", &session, "--append-system-prompt"]);
    assert_eq!(
        argv[3..].join("\n"),
        "Keep it short.\n\nSay why.",
        "the project's instructions, then the local ones (argv is saved a line per argument)"
    );
}

#[test]
fn threads_resolve_in_the_current_repository_only() {
    let s = Sandbox::new();
    let other = s.root.join("other");
    s.init(&other);
    s.transcript("s-1", &ran_in(&talk(2, |k| format!("here {k}")), &s.repo()));
    s.transcript_in(
        &other,
        "s-2",
        &ran_in(&talk(1, |k| format!("there {k}")), &other),
    );
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(
        s.ai_at(&other, "s-2", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(
        s.ai_at(&other, "s-2", &["attach", "dev"]),
        "Attached to dev.\n",
    );
    let list = |dir: &Path| stdout(&s.cmd(dir).arg("list").output().unwrap());
    assert!(list(&s.repo()).contains("  s-1  ") && !list(&s.repo()).contains("s-2"));
    assert!(list(&other).contains("  s-2  ") && !list(&other).contains("s-1"));
    let here = stdout(&s.ai(&["zoom", "-a", "dev", "0.0"]));
    let there = stdout(
        &s.cmd(&other)
            .args(["zoom", "-a", "dev", "0.0"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        (here.as_str(), there.as_str()),
        ("0.0|user: here 0\n", "0.0|user: there 0\n")
    );
    let o = s.cmd(&other).args(["resume", "dev"]).output().unwrap();
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""));
    assert_eq!(s.argv(), ["--resume", "s-2"]);
    assert_eq!(s.claude_dir(), other);
}

#[test]
fn build_writes_rewrites_and_dry_runs_the_lines() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(3, |k| format!("question {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    s.line("dev", 1, 1, "first, by hand");
    let fresh = "0|user: question 0 ai: reply 0\n";
    let o = s.ai_in("s-1", &["build", "--dry-run", "--model", "opus"]);
    let dry = stdout(&o);
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""));
    assert!(
        !dry.contains(fresh),
        "the line written by hand stays: {dry}"
    );
    assert!(
        dry.ends_with("Dry run: 4 lines, 0 by the model; nothing stored.\n"),
        "{dry}"
    );
    let all = |s: &Sandbox| {
        let t = s.store().thread("dev").unwrap();
        t.snapshot().unwrap().all_lines().unwrap().len()
    };
    assert_eq!(all(&s), 1, "a dry run stores nothing");
    let log = fs::read_to_string(s.store().dir().join("dev.log")).unwrap();
    assert!(
        log.contains("dry_run{pid=") && log.contains("model=\"opus\""),
        "{log}"
    );

    ok(
        s.ai_in("s-1", &["build"]),
        "Wrote 4 lines, 0 by the model.\n",
    );
    ok(s.ai(&["build", "-a", "dev"]), "Nothing to write.\n");
    let o = s.ai_in("s-1", &["build", "--rebuild", "--dry-run"]);
    let dry = stdout(&o);
    assert!(
        dry.starts_with(&format!(
            "0.1|ai: reply 0\n  was: ai: reply 0\n{fresh}  was: first, by hand\n"
        )),
        "{dry}"
    );
    assert!(dry.ends_with("Dry run: 5 lines, 0 by the model; nothing stored.\n"));
    ok(
        s.ai_in("s-1", &["build", "--rebuild"]),
        "Dropped 5 lines; wrote 5, 0 by the model.\n",
    );
    let t = s.store().thread("dev").unwrap();
    let line = t
        .snapshot()
        .unwrap()
        .line(Key { level: 1, last: 1 })
        .unwrap()
        .unwrap();
    assert_eq!(format!("0|{}\n", line.text), fresh);
}

#[test]
fn build_writes_or_rewrites_one_line() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(5, |k| format!("question {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let ends = turn_ends(&s, "dev");
    s.line("dev", 1, ends[0], "by hand");
    ok(
        s.ai_in("s-1", &["build", "0+2"]),
        "Wrote 3 lines, 0 by the model.\n",
    );
    ok(
        s.ai_in("s-1", &["build", "0"]),
        "0 is written already. Rewrite it with --rebuild.\n",
    );
    ok(
        s.ai_in("s-1", &["build", "0", "--rebuild", "--dry-run"]),
        "0.1|ai: reply 0\n0|user: question 0 ai: reply 0\n  was: by hand\n\
         Dry run: 2 lines, 0 by the model; nothing stored.\n",
    );
    let dry = stdout(&s.ai(&["build", "-a", "dev", "2+2", "--dry-run"]));
    let ids: Vec<&str> = dry.lines().map(|l| l.split('|').next().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "2.1",
            "2",
            "3.1",
            "3",
            "2+2",
            "Dry run: 5 lines, 0 by the model; nothing stored."
        ],
        "{dry}"
    );
    ok(
        s.ai_in("s-1", &["build"]),
        "Wrote 7 lines, 0 by the model.\n",
    );
    ok(
        s.ai_in("s-1", &["build", "0", "--rebuild"]),
        "Dropped 3 lines; wrote 1, 0 by the model.\n\
         The next ai agent build -a dev writes the 2 lines above 0 again.\n",
    );
    let t = s.store().thread("dev").unwrap();
    let snap = t.snapshot().unwrap();
    let line = |level, last| snap.line(Key { level, last }).unwrap();
    assert_eq!(
        line(1, ends[0]).unwrap().text,
        "user: question 0 ai: reply 0"
    );
    assert!(line(2, ends[1]).is_none() && line(3, ends[3]).is_none());
    assert!(line(2, ends[3]).is_some(), "2+2 is not above 0");
    ok(
        s.ai_in("s-1", &["build"]),
        "Wrote 2 lines, 0 by the model.\n",
    );
    fails(
        s.ai_in("s-1", &["build", "4"]),
        "4 is not a line on this branch. Lines are closed turns (12), aligned nodes over closed turns (8+4) and their work (12.1+7).\n",
    );
    fails(
        s.ai_in("s-1", &["build", "1+3"]),
        "1+3 is not a line on this branch. Lines are closed turns (12), aligned nodes over closed turns (8+4) and their work (12.1+7).\n",
    );
    fails(
        s.ai_in("s-1", &["build", "x"]),
        "'x' is not an id. Ids look like 12 (a turn), 8+4 (turns 8 to 11) or 12.1+7 (the work of turn 12).\n",
    );
}

/// A question too long to be its own line, so its turn's line takes a call.
fn long(tag: &str, k: usize) -> String {
    format!("{tag} question {k} {}", "q".repeat(600))
}

/// Thread dev, attached to session s-1: `n` closed turns that each take a call, the first
/// failing, and one open.
fn failing(s: &Sandbox, n: usize) -> Vec<u64> {
    s.fake_compactor("0.5");
    let tag = |k| if k == 0 { "BAD" } else { "ok" };
    s.transcript("s-1", &talk(n + 1, |k| long(tag(k), k)));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    turn_ends(s, "dev")
}

/// Whether each of the first `n` turns of dev has its line.
fn built(s: &Sandbox, ends: &[u64], n: usize) -> Vec<bool> {
    ends[..n].iter().map(|&e| s.has_line("dev", 1, e)).collect()
}

#[test]
fn build_stops_at_the_first_line_that_fails() {
    let s = Sandbox::new();
    let ends = failing(&s, 10);
    let o = s.ai_in("s-1", &["build"]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    assert!(
        stdout(&o).ends_with("Failed 0: claude: overloaded\n"),
        "{}",
        stdout(&o)
    );
    assert_eq!(
        built(&s, &ends, 10),
        [
            false, true, true, true, true, true, true, true, false, false
        ],
        "8 calls run at once: the 7 running beside the failure are kept, the rest never start"
    );
}

#[test]
fn the_background_summariser_builds_past_a_failed_line() {
    let s = Sandbox::new();
    let ends = failing(&s, 10);
    let o = s.ai(&["build", "-a", "dev", "--background"]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    assert_eq!(
        built(&s, &ends, 10),
        [false, true, true, true, true, true, true, true, true, true]
    );
    let o = s.ai(&["build", "-a", "dev", "--background", "--rebuild"]);
    assert_eq!(code(&o), 2, "only for the background run");
}

#[test]
fn attaching_starts_a_background_summariser_that_builds() {
    let s = Sandbox::new();
    fs::write(
        s.repo().join("ai.local.toml"),
        "[chat]\nbackground = true\n",
    )
    .unwrap();
    s.fake_compactor("0");
    s.transcript("s-1", &talk(3, |k| long("ok", k)));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let ends = turn_ends(&s, "dev");
    let log = s.store().thread("dev").unwrap().log_path().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let text = fs::read_to_string(&log).unwrap_or_default();
        if text.contains(" end ") {
            break;
        }
        assert!(!text.contains("error:"), "the summariser failed: {text}");
        assert!(
            Instant::now() < deadline,
            "the summariser never ended: {text}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(built(&s, &ends, 2), [true, true]);
}

#[test]
fn build_off_a_terminal_shows_no_progress() {
    let s = Sandbox::new();
    s.fake_compactor("0.2");
    s.transcript("s-1", &talk(3, |k| long("ok", k)));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(
        s.ai_in("s-1", &["build"]),
        "Wrote 5 lines, 2 by the model.\n",
    );
}

#[test]
fn a_line_still_over_the_limit_fails_and_is_not_stored() {
    let s = Sandbox::new();
    s.fake_compactor("0");
    s.transcript("s-1", &talk(2, |k| long("LONG", k)));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let ends = turn_ends(&s, "dev");
    let o = s.ai_in("s-1", &["build"]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    assert!(
        stdout(&o).ends_with("Failed 0: 600 bytes after 5 tries; this line holds at most 512\n"),
        "{}",
        stdout(&o)
    );
    assert!(!s.has_line("dev", 1, ends[0]));
}

#[test]
fn a_failed_line_stops_compaction_and_a_new_session() {
    let s = Sandbox::new();
    failing(&s, 20);
    let failed = "Cannot build the view of thread dev: 1 line failed.\n\
                  Failed 0: claude: overloaded\n\
                  Write them, then try again: ai agent build -a dev\n";
    fails(s.ai(&["compact-session", "--session", "s-1"]), failed);
    fails(
        s.ai(&["resume", "dev"]),
        &format!(
            "Cannot resume dev: the transcript of session s-1 names no directory. Starting a \
             new session.\n{failed}"
        ),
    );
    assert!(
        !s.root.join("out/argv").exists(),
        "Claude Code did not start"
    );
    let t = s.store().thread("dev").unwrap();
    assert_eq!(
        t.snapshot().unwrap().bindings().unwrap().len(),
        1,
        "no session was bound"
    );
}

#[test]
fn an_ai_toml_without_an_id_has_no_threads() {
    let s = Sandbox::new();
    let bare = s.root.join("bare");
    fs::create_dir(&bare).unwrap();
    fs::write(bare.join("ai.toml"), "[summariser]\nmodel = \"haiku\"\n").unwrap();
    let out = s.ai_at(&bare, "s-1", &["attach", "dev"]);
    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "{0}/ai.toml has no project id. Run `ai init` in {0}.\n",
            bare.display()
        )
    );
}

#[test]
fn the_store_is_a_git_repository_committed_on_events() {
    let s = Sandbox::new();
    let store = s.repo().join(".ai/local");
    ok(
        s.ai(&["list"]),
        "No agents in this project. Create one: ai agent new <name>\n",
    );
    assert!(!store.join(".git").exists(), "a read makes no repository");
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    assert_eq!(
        fs::read_to_string(store.join(".gitignore")).unwrap(),
        "*.lock\n*.log\n"
    );
    let log = || git(&store, &["log", "--format=%an: %s"]);
    assert_eq!(log(), "ai: ai agent new dev\n");

    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    assert_eq!(
        log(),
        "ai: ai agent attach dev\nai: ai agent new dev\n",
        "a write that changes nothing makes no commit"
    );
    let files = git(&store, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(files.contains("agents/threads/dev/items.jsonl"), "{files}");
    assert!(store.join("agents/dev.write.lock").is_file());
    assert!(!files.contains(".lock"), "{files}");
    assert_eq!(git(&store, &["status", "--porcelain"]), "");

    fs::write(store.join(".git/HEAD"), "not a ref\n").unwrap();
    let o = s.ai(&["new", "other"]);
    assert_eq!(code(&o), 1);
    assert!(
        stderr(&o).starts_with(&format!("Cannot commit {}: ", store.display())),
        "{}",
        stderr(&o)
    );
}

#[test]
fn a_turns_sync_waits_for_the_next_event_to_be_committed() {
    let s = Sandbox::new();
    s.fake_compactor("0");
    assert_eq!(code(&s.top(None, &["memory", "init"])), 0);
    let store = s.repo().join(".ai/local");
    let log = || git(&store, &["log", "--format=%s"]);
    let clean = || git(&store, &["status", "--porcelain"]).is_empty();
    let turns = |n| s.transcript("s-1", &talk(n, |k| long("ok", k)));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    turns(1);
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let events = log();

    turns(3);
    ok(s.ai_in("s-1", &["sync-session"]), "");
    turns(4);
    assert!(s.ai_in("s-1", &["view"]).status.success());
    turns(5);
    assert!(s.top(Some("s-1"), &["view", "--whole"]).status.success());
    assert!(
        s.ai(&["build", "-a", "dev", "--background"])
            .status
            .success()
    );
    assert!(s.has_line("dev", 1, turn_ends(&s, "dev")[0]));
    assert_eq!(
        log(),
        events,
        "a turn's sync, a view and the background summariser commit nothing"
    );
    assert!(!clean(), "what they wrote waits on disk");
    ok(s.ai_in("s-1", &["end-session"]), "");
    assert_eq!(log(), format!("ai agent end-session\n{events}"));
    assert!(clean(), "the next event commits it");

    let top = || log().lines().next().unwrap().to_string();
    for (n, args, msg) in [
        (6, &["compact-session"][..], "ai agent compact-session"),
        (7, &["build"], "ai agent build"),
        (8, &["detach"], "ai agent detach"),
    ] {
        turns(n);
        ok(s.ai_in("s-1", &["sync-session"]), "");
        let o = s.ai_in("s-1", args);
        assert!(o.status.success(), "{msg}: {}", stderr(&o));
        assert_eq!(top(), msg);
        assert!(clean(), "{msg} commits the turn synced before it");
    }
    s.ai(&["resume", "dev"]);
    assert_eq!(top(), "ai agent resume dev");
}

#[test]
fn a_linked_worktree_commits_to_the_main_checkouts_store() {
    let s = Sandbox::new();
    git(&s.repo(), &["init", "-q"]);
    git(&s.repo(), &["add", "."]);
    git(&s.repo(), &["commit", "-qm", "init"]);
    let wt = s.root.join("wt");
    git(&s.repo(), &["worktree", "add", "-q", wt.to_str().unwrap()]);
    ok(
        s.cmd(&wt).args(["new", "dev"]).output().unwrap(),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    assert!(!wt.join(".ai").exists());
    let store = s.repo().join(".ai/local");
    assert_eq!(git(&store, &["log", "--format=%s"]), "ai agent new dev\n");
    assert_eq!(git(&s.repo(), &["status", "--porcelain"]), "");
}
