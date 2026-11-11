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
    config::Homes,
    memory::Who,
    project::Project,
    repo,
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
        Project::init_in(dir, &self.homes()).unwrap();
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// The user's homes, which `ai` finds from this sandbox's `HOME`.
    fn homes(&self) -> Homes {
        Homes::at(&self.root.join("home"))
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
    /// directory in `out/cwd`; as `claude agents`, it answers as [`AGENTS`] says.
    fn fake_claude(&self) {
        let path = std::env::var("PATH").unwrap();
        let bash = std::env::split_paths(&path)
            .map(|d| d.join("bash"))
            .find(|p| p.is_file())
            .expect("bash on PATH");
        let src = self.root.join("claude.sh");
        let out = self.root.join("out");
        let body = format!(
            "#!{}\n{}\nprintf '%s\\n' \"$@\" > {}\npwd > {}\n",
            bash.display(),
            agents(&out),
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
    /// holding `BAD` fails, one holding `LONG` gets a 600-byte line each time, one holding
    /// `SLOW` never answers while `out/hang` exists (its pid in `out/slow.pid`), any other gets
    /// a short line after `delay` seconds. Each message it gets adds a line to `out/calls`.
    /// Started as Claude Code, it records as [`Sandbox::fake_claude`].
    fn fake_compactor(&self, delay: &str) {
        let path = std::env::var("PATH").unwrap();
        let bash = std::env::split_paths(&path)
            .map(|d| d.join("bash"))
            .find(|p| p.is_file())
            .expect("bash on PATH");
        let out = self.root.join("out");
        let body = format!(
            r#"#!{bash}
{agents}
if [ "$1" != "-p" ]; then
  printf '%s\n' "$@" > {argv}
  pwd > {cwd}
  exit 0
fi
long=$(printf '%0600d' 0)
mode=
while IFS= read -r l; do
  echo >> {out}/calls
  if [ -z "$mode" ]; then
    case "$l" in
      *BAD*) mode=bad ;;
      *LONG*) mode=long ;;
      *SLOW*) mode=slow ;;
      *) mode=good ;;
    esac
  fi
  if [ $mode = slow ] && [ -e {out}/hang ]; then
    echo $$ > {out}/slow.pid
    exec sleep 600
  fi
  case $mode in
    bad) printf '%s\n' '{{"type":"result","is_error":true,"result":"overloaded"}}' ;;
    long) printf '{{"type":"result","result":"%s"}}\n' "$long" ;;
    good|slow) sleep {delay}; printf '%s\n' '{{"type":"result","result":"a line"}}' ;;
  esac
done
"#,
            bash = bash.display(),
            agents = agents(&out),
            out = out.display(),
            argv = out.join("argv").display(),
            cwd = out.join("cwd").display(),
        );
        let dst = self.root.join("bin/claude");
        fs::remove_file(&dst).unwrap();
        fs::write(&dst, body).unwrap();
        fs::set_permissions(&dst, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }

    /// The messages [`Sandbox::fake_compactor`] has answered.
    fn calls(&self) -> usize {
        fs::read_to_string(self.root.join("out/calls")).map_or(0, |c| c.lines().count())
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
            .env("XDG_CACHE_HOME", self.root.join("cache"))
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
        Store::find_in(&self.homes(), &self.repo())
            .unwrap()
            .unwrap()
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

/// How the fake `claude` answers `claude agents --json`, the sessions it runs: the contents of
/// `out/agents-<name>.json` for the configuration directory `<name>` it is asked under (`usual`
/// when the environment names none), else of `out/agents.json`, else none; failing, with `boom`
/// on stderr, while `out/agents.fail` exists.
const AGENTS: &str = r#"if [ "$1" = agents ]; then
  if [ -e {out}/agents.fail ]; then echo boom >&2; exit 1; fi
  cat "{out}/agents-$(basename "${CLAUDE_CONFIG_DIR:-usual}").json" 2>/dev/null \
    || cat {out}/agents.json 2>/dev/null || echo '[]'
  exit 0
fi"#;

/// [`AGENTS`] for the sandbox's `out` directory.
fn agents(out: &Path) -> String {
    AGENTS.replace("{out}", &out.display().to_string())
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

/// Exit 0, with a build's plan and lines, then an end that says `want`: its first line
/// without the time it took, then what follows its log.
fn build_ends(o: Output, want: &str) {
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""), "{}", stdout(&o));
    let out = stdout(&o);
    let end =
        Regex::new(r"(?s)\nBuilt (\d+ lines?) in [^:]+: ([^\n]*)\n[^\n]*\nLog: [^\n]*\n(.*)$")
            .unwrap();
    let c = end
        .captures(&out)
        .unwrap_or_else(|| panic!("no end: {out}"));
    assert_eq!(
        format!("Built {}: {}\n{}", &c[1], &c[2], &c[3]),
        want,
        "{out}"
    );
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

/// The lines of one section of a view, each item under a turn's number with its whole id.
fn section(view: &str, title: &str) -> Vec<String> {
    let Some((_, rest)) = view.split_once(&format!("\n## {title}\n")) else {
        return Vec::new();
    };
    let mut turn = "";
    let mut out = Vec::new();
    for l in rest.split("\n\n## ").next().unwrap().lines() {
        match l {
            _ if !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()) => turn = l,
            _ if l.starts_with('.') => out.push(format!("{turn}{l}")),
            _ => out.push(l.to_string()),
        }
    }
    out
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
        "carry-session",
        "session-name",
        "nap",
        "claude",
    ] {
        assert!(!help.contains(cmd), "{cmd}: {help}");
    }
    for cmd in ["list", "view", "attach", "detach", "zoom", "grep", "build"] {
        let help = stdout(&s.ai(&[cmd, "--help"]));
        for flag in ["--harness", "--session", "--transcript"] {
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

/// A transcript's first row as `/fold` leaves it: the view the plugin appended, in a session
/// started in `dir`.
fn view_row(dir: &Path) -> String {
    let row = json!({
        "type": "user", "isMeta": true, "uuid": "v1", "parentUuid": null,
        "timestamp": "2026-10-14T17:00:00.000Z", "cwd": dir,
        "origin": {"kind": "plugin", "name": "ai"},
        "message": {"role": "user", "content": [{"type": "text", "text": "You are awake."}]},
    });
    format!("{row}\n")
}

#[test]
fn carry_session_attaches_a_session_where_the_one_before_it_ends() {
    const AGENT: &str = "a1b2c3d4e5f6a7b8c";
    let s = Sandbox::new();
    let all = ran_in(&fixture("subagent.jsonl"), &s.repo());
    let part: String = all.split_inclusive('\n').take(5).collect();
    let old = s.transcript("s-1", &part);
    let sub = fixture(&format!("subagent/subagents/agent-{AGENT}.jsonl"));
    let lines: Vec<&str> = sub.split_inclusive('\n').collect();
    let agent = |session: &str, text: String| {
        let dir = s.project(&s.repo()).join(session).join("subagents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("agent-{AGENT}.jsonl")), text).unwrap();
        let meta = format!("subagent/subagents/agent-{AGENT}.meta.json");
        fs::write(dir.join(format!("agent-{AGENT}.meta.json")), fixture(&meta)).unwrap();
    };
    agent("s-1", lines[..2].concat());
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");

    fs::write(&old, &all).unwrap();
    agent("s-1", lines[..4].concat());
    agent("s-2", lines[4..].concat());
    s.transcript("s-2", &view_row(&s.repo()));
    ok(
        s.ai_in("s-2", &["carry-session", "--from", "s-1"]),
        "Attached to dev.\n",
    );
    let t = s.store().thread("dev").unwrap();
    let snap = t.snapshot().unwrap();
    let binding = |id: &str| snap.binding("claude", id).unwrap().unwrap();
    let (before, after) = (binding("s-1"), binding("s-2"));
    assert!(before.is_attached() && after.is_attached());
    let heads = snap.heads().unwrap();
    let last = heads.iter().rfind(|h| h.session == "s-1").unwrap();
    assert_eq!(
        (last.entry.as_str(), before.tip),
        ("a3", Some(last.seq)),
        "the session before is synced to its end first"
    );
    assert_eq!(
        (after.graft, after.tip),
        (before.tip, None),
        "it goes on from that session's branch"
    );
    let items = snap.agent_items("claude", AGENT).unwrap();
    let chain: Vec<(&str, &str, Option<&str>)> = items
        .iter()
        .map(|i| {
            let parent = i.parent.as_ref().map(|p| p.entry.as_str());
            (i.id.session.as_str(), i.id.entry.as_str(), parent)
        })
        .collect();
    assert_eq!(
        chain,
        [
            ("s-1", "s1", None),
            ("s-1", "sa1", Some("s1")),
            ("s-1", "sr1", Some("sa1")),
            ("s-2", "sa2", Some("sr1")),
            ("s-2", "c1", Some("sa2")),
            ("s-2", "sa3", Some("c1")),
        ],
        "a subagent that ran on through the clear is one agent, each entry once"
    );
    assert!(snap.agent("claude", AGENT).unwrap().unwrap().call.is_some());
    let mark = snap.mark();
    ok(s.ai_in("s-2", &["sync-session"]), "");
    ok(s.ai_in("s-1", &["sync-session"]), "");
    assert_eq!(t.snapshot().unwrap().mark(), mark, "a sync adds nothing");
    assert_eq!(
        t.snapshot().unwrap().agent_items("claude", AGENT).unwrap(),
        items
    );

    ok(s.ai(&["resume", "dev"]), "");
    assert_eq!(
        s.argv(),
        ["--resume", "s-2", "--name=repo.dev"],
        "the session that goes on comes before the one it took over from"
    );
    let turn = talk(1, |_| "go on".into())
        .replace("\"u0\"", "\"n0\"")
        .replace("\"parentUuid\":null", "\"parentUuid\":\"v1\"");
    s.transcript("s-2", &(view_row(&s.repo()) + &ran_in(&turn, &s.repo())));
    ok(s.ai_in("s-2", &["sync-session"]), "");
    let snap = t.snapshot().unwrap();
    let first = snap
        .heads()
        .unwrap()
        .into_iter()
        .find(|h| h.session == "s-2");
    assert_eq!(
        first.map(|h| (h.entry, h.parent)),
        Some(("n0".into(), before.tip)),
        "the view is no item: the first message hangs on the session before"
    );

    ok(
        s.ai_in("s-5", &["carry-session", "--from", "s-1"]),
        "Attached to dev.\n",
    );
    let snap = t.snapshot().unwrap();
    assert_ne!(before.tip, snap.graft().unwrap());
    assert_eq!(
        snap.binding("claude", "s-5").unwrap().unwrap().graft,
        before.tip,
        "from that session's branch, not the thread's newest item"
    );
    ok(
        s.ai_in("s-2", &["carry-session", "--from", "s-1"]),
        "Attached to dev.\n",
    );
    assert_eq!(
        t.snapshot().unwrap().binding("claude", "s-2").unwrap(),
        snap.binding("claude", "s-2").unwrap(),
        "carried again, a session stays where it is"
    );
    unattached(s.ai_in("s-3", &["carry-session", "--from", "s-8"]));
    assert!(s.store().attached("claude", "s-3").unwrap().is_none());
    unattached(s.outside(&["carry-session", "--from", "s-1", "--session", "s-3"]));
    fails(
        s.ai(&["carry-session", "--from", "s-1"]),
        "No session: run this inside an agent session, or pass --session.\n",
    );
    fails(
        s.ai_in("s-1", &["carry-session", "--from", "s-1"]),
        "Session claude s-1 cannot go on from itself.\n",
    );
    ok(
        s.ai(&["new", "other"]),
        "Created agent other. Start it: ai agent resume other\n",
    );
    ok(s.ai_in("s-4", &["attach", "other"]), "Attached to other.\n");
    fails(
        s.ai_in("s-4", &["carry-session", "--from", "s-1"]),
        "Session claude s-4 is attached to other. Run ai agent detach --harness claude --session s-4 before attaching to dev.\n",
    );
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
    assert!(
        view.ends_with(
            "\n3\n.0|user: Show me the diff instead.\n.1|ai: The diff changes the test filter only.\n"
        ),
        "under its number: {view}"
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
    let text = |k: usize| format!("{k} {}", "w".repeat(700));
    for (k, &end) in ends.iter().enumerate() {
        s.line("long-one", 1, end, &text(k));
    }
    // The 96 turns before the recent three fold to 60 lines: the 24 oldest in nodes of 4, the
    // next 36 in pairs.
    for i in 0..30 {
        s.line("long-one", 2, ends[2 * i + 1], &text(i));
    }
    for i in 0..6 {
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
    let (header, body, next) = part(&first);
    assert_eq!(header, "ai agent view long-one, part 1 of 3.");
    assert_eq!(
        part(&stdout(&s.ai(&["view", "long-one"]))).1,
        body,
        "outside the session: the thread's newest branch, here the same"
    );
    let mut parts = body.to_string();
    s.transcript("s-2", &talk(101, ask));
    ok(s.ai_in("s-2", &["sync-session"]), "");
    assert_ne!(
        paged(&s, Some("s-2"), &["agent", "view"]),
        view,
        "the thread moved on"
    );
    let second = stdout(&s.top(None, &["page", next.unwrap()]));
    let (header, body, next) = part(&second);
    assert_eq!(header, "ai agent view long-one, part 2 of 3.");
    parts += body;
    let third = stdout(&s.top(None, &["page", next.unwrap()]));
    let (header, body, next) = part(&third);
    assert_eq!(
        (header, next),
        ("ai agent view long-one, part 3 of 3.", None)
    );
    parts += body;
    assert_eq!(parts, view, "the parts come from one state");
    fails(
        s.top(None, &["page", "99:1"]),
        "View 99 is gone: run the command again.\n",
    );
    fails(
        s.top(None, &["page", "2"]),
        "'2' is not a page. Copy it as printed, like 17:2.\n",
    );
}

/// A part of paged output: its header, its body and the page its `Next` line names, if any.
fn part(out: &str) -> (&str, &str, Option<&str>) {
    let (header, rest) = out.split_once('\n').expect(out);
    match rest.rsplit_once("Next: ai page ") {
        Some((body, next)) => (header, body, Some(next.trim_end())),
        None => (header, rest, None),
    }
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
        alone.ends_with("\nYou are awake.\n"),
        "with no thread, only the memory: {alone}"
    );
    assert_eq!(
        paged(&s, None, &["view"]),
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
    assert_eq!(
        format!("{mem}You are awake.\n"),
        alone,
        "the memory's closing line moves after the thread"
    );
    assert!(
        thread.contains("|user: question 1") && thread.ends_with("\n\nYou are awake.\n"),
        "{thread}"
    );
    let first = stdout(&s.top(Some("s-1"), &["view"]));
    assert!(
        first.starts_with("ai view dev, part 1 of 2.\n# Memory\n"),
        "{first}"
    );
    assert!(
        Regex::new(r"\nNext: ai page \d+:2\n$")
            .unwrap()
            .is_match(&first),
        "{first}"
    );
    assert_eq!(paged(&s, Some("s-1"), &["view"]), both);
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
            .filter(|l| l.contains(r#""cmd":"view","args":["--whole"]"#))
            .count(),
        8,
        "each ai view reads the memory once, after its thread is known: {usage}"
    );
}

/// The output of `args` run as `ai` in `session`, then of each `ai page` its parts name: the
/// parts joined, without their headers and `Next` lines.
fn paged(s: &Sandbox, session: Option<&str>, args: &[&str]) -> String {
    let header = Regex::new(r"^.*, part \d+ of \d+[.,].*\n").unwrap();
    let mut out = String::new();
    let mut o = s.top(session, args);
    loop {
        assert_eq!(code(&o), 0, "{}", stderr(&o));
        let text = stdout(&o);
        let Some(h) = header.find(&text) else {
            return out + &text;
        };
        let body = &text[h.end()..];
        let Some((body, next)) = body.rsplit_once("Next: ai page ") else {
            return out + body;
        };
        out += body;
        o = s.top(None, &["page", next.trim_end()]);
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
    let both = |args: &[&str], command: &str| -> String {
        let out = stdout(&s.ai_in("s-1", args));
        let body = out
            .strip_prefix(&format!("{command}, part 1 of 2.\n"))
            .expect(&out);
        let (body, next) = body.split_once("\nNext: ai page ").expect(&out);
        let rest = stdout(&s.top(None, &["page", next.trim_end()]));
        let rest = rest
            .strip_prefix(&format!("{command}, part 2 of 2.\n"))
            .expect(&rest);
        format!("{body}{rest}")
    };
    assert_eq!(
        both(&["zoom", "4.0"], "ai agent zoom -a dev 4.0"),
        format!("4.0|user: {}\n", "paste ".repeat(5_000).trim_end())
    );
    both(
        &["zoom", "4", "--tool-output"],
        "ai agent zoom -a dev 4 --tool-output",
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
    let mut out = stdout(&s.ai_in("s-1", &["grep", "Question", "-i"]));
    let header = Regex::new(
        r"^ai agent grep -a dev 'Question' -i, part (\d+) of (\d+), newest first: (\d+) of 300 matches\.$",
    )
    .unwrap();
    let (mut k, mut view) = (0, String::new());
    loop {
        k += 1;
        let (head, body, next) = part(&out);
        let page: Vec<&str> = body.lines().collect();
        assert!(body.len() < 20_000);
        seen.splice(
            0..0,
            page.iter()
                .map(|l| l.split('|').next().unwrap().to_string()),
        );
        let caps = header.captures(head).expect(head);
        assert_eq!(
            (&caps[1], &caps[3]),
            (k.to_string().as_str(), page.len().to_string().as_str())
        );
        let Some(next) = next else {
            assert_eq!(&caps[2], k.to_string());
            break;
        };
        let (v, p) = next.split_once(':').unwrap();
        assert_eq!(p, (k + 1).to_string());
        view = v.to_string();
        out = stdout(&s.top(None, &["page", next]));
    }
    assert!(k > 1);
    let want: Vec<String> = (0..300).map(|k| format!("{k}.0")).collect();
    assert_eq!(seen, want, "every match once, oldest first");
    fails(
        s.top(None, &["page", &format!("{view}:{}", k + 1)]),
        &format!("View {view} has no part {}: it has {k}.\n", k + 1),
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
    let mut out = stdout(&s.ai(&["grep", "-a", "one", "-a", "two", "says"]));
    let mut seen = Vec::new();
    let mut parts = 0;
    for k in 1.. {
        parts = k;
        let (header, body, next) = part(&out);
        let want = format!("ai agent grep -a one -a two 'says', part {k} of ");
        assert!(header.starts_with(&want), "{header}");
        seen.splice(
            0..0,
            body.lines()
                .map(|l| l.split('|').next().unwrap().to_string()),
        );
        let Some(next) = next else {
            break;
        };
        out = stdout(&s.top(None, &["page", next]));
    }
    let want: Vec<String> = ["one", "two"]
        .iter()
        .flat_map(|a| (0..150).map(move |k| format!("{a} {k}.0")))
        .collect();
    assert!(parts > 1);
    assert_eq!(seen, want, "every match once, agent by agent");
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
    assert_eq!(argv[2..], ["--name=repo.dev", "--model", "haiku"]);
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
    let from_home = want.strip_prefix(s.root.join("claude")).unwrap();
    assert!(
        b.transcript
            .as_deref()
            .is_some_and(|p| p.ends_with(from_home)),
        "the transcript is recorded from Claude Code's home, as {from_home:?}: {:?}",
        b.transcript
    );
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
            "Cannot resume dev: the transcript of session {id} is gone, and there is no backup \
             to bring it back from: `ai backup run` makes one. Starting a new session.\n"
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
    assert_eq!(
        s.argv(),
        ["--resume", "s-2", "--name=repo.dev", "--model", "haiku"]
    );
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
    assert_eq!(s.argv(), ["--resume", "s-2", "--name=repo.dev"]);
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
        "Cannot resume dev: the transcript of session s-2 is gone, and there is no backup to \
         bring it back from: `ai backup run` makes one. Starting a new session.\n"
    );
    let argv = s.argv();
    assert_eq!(argv[0], "--session-id");
    assert_eq!(argv[2..], ["--name=repo.dev", "--model", "haiku"]);
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
    assert_eq!(
        s.argv(),
        ["--resume", "s-1", "--name=repo.dev", "--model", "haiku"]
    );
    assert_eq!(s.claude_dir(), s.repo());
}

#[test]
fn a_session_is_named_by_its_agents_address() {
    let s = Sandbox::new();
    fails(
        s.ai(&["session-name", "dev"]),
        "No agent dev in this project. Create it: ai agent new dev\n",
    );
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai(&["session-name", "dev"]), "repo.dev\n");
    ok(s.ai(&["resume", "dev", "--", "--name", "mine"]), "");
    assert_eq!(
        s.argv()[2..],
        ["--name=repo.dev", "--name", "mine"],
        "the user's --name comes last, so Claude Code keeps it"
    );
    let o = s.ai(&["session-name", "a.b"]);
    assert_eq!(code(&o), 1);
    assert!(
        stderr(&o).contains("is not a thread name"),
        "{}",
        stderr(&o)
    );

    let dotted = s.root.join("quran.thayyil.net");
    s.init(&dotted);
    let at = |args: &[&str]| s.cmd(&dotted).args(args).output().unwrap();
    ok(
        at(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(at(&["session-name", "dev"]), "quran.thayyil.net.dev\n");

    let long = "l".repeat(196);
    ok(
        s.ai(&["new", &long]),
        &format!("Created agent {long}. Start it: ai agent resume {long}\n"),
    );
    let refused = format!(
        "Claude Code cuts a session's name at 200 characters, and repo.{long} has 201. Give the \
         agent, or the project's folder, a shorter name.\n"
    );
    fails(s.ai(&["session-name", &long]), &refused);
    fails(s.ai(&["resume", &long]), &refused);
    assert!(!s.root.join("out/argv").exists(), "nothing started");
    let t = s.store().thread(&long).unwrap();
    assert!(
        t.snapshot().unwrap().bindings().unwrap().is_empty(),
        "no session was bound"
    );
    let fits = "f".repeat(195);
    ok(
        s.ai(&["new", &fits]),
        &format!("Created agent {fits}. Start it: ai agent resume {fits}\n"),
    );
    ok(s.ai(&["session-name", &fits]), &format!("repo.{fits}\n"));

    let folder = s.homes().repo.join("projects/repo");
    let odd = s.homes().repo.join("projects/re po");
    fs::rename(&folder, &odd).unwrap();
    fails(
        s.ai(&["session-name", "dev"]),
        "The project's folder ~/ai/projects/re po has a name an agent's address can't begin \
         with: use letters, digits, '.', '-' and '_'. Rename the folder; ai finds it by its id.\n",
    );
    fs::rename(&odd, s.homes().repo.join("projects/renamed")).unwrap();
    ok(s.ai(&["session-name", "dev"]), "renamed.dev\n");
}

#[test]
fn a_broken_hierarchy_or_another_agents_table_stops_no_agent_command() {
    let s = Sandbox::new();
    s.transcript("s-1", &ran_in(&fixture("plain.jsonl"), &s.repo()));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let view = stdout(&s.ai_in("s-1", &["view"]));
    let toml = s.repo().join("ai.toml");
    let project = fs::read_to_string(&toml).unwrap();
    fs::write(
        &toml,
        format!(
            "{project}\n[summariser]\nmodle = \"haiku\"\n\n[agents.dev]\nreports-to = [\"lead\"]\n\n\
             [agents.lead]\nreports-to = [\"dev\"]\n\n[agents.qa]\nreports-to = [\"nobody\"]\n\
             worktree = \"\"\n"
        ),
    )
    .unwrap();
    ok(s.ai_in("s-1", &["view"]), &view);
    ok(s.ai_in("s-1", &["sync-session"]), "");
    let listed = stdout(&s.ai(&["list"]));
    assert!(listed.contains("dev"), "{listed}");
    ok(s.ai(&["resume", "dev"]), "");
    assert_eq!(s.argv()[..2], ["--resume", "s-1"]);
    ok(
        s.ai(&["new", "qa"]),
        "Created agent qa. Start it: ai agent resume qa\n",
    );
    let o = s.ai(&["resume", "qa"]);
    let err = stderr(&o);
    assert!(
        code(&o) == 1
            && err.starts_with(&format!("{}: ", toml.display()))
            && err.contains("worktree")
            && err.contains("is empty"),
        "qa's own table stops qa: {err}"
    );
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
        "Agent dev's worktree {} does not share this project's agents: make it a checkout \
         of {}, holding an ai.toml with the id {id}.\n",
        wt.display(),
        s.repo().display()
    );
    fails(s.ai(&["resume", "dev"]), &unshared);
    fs::write(wt.join("ai.toml"), &project).unwrap();
    fails(
        s.ai(&["resume", "dev"]),
        &format!(
            "Cannot read agent dev's instructions: {}: No such file or directory.\n",
            s.repo().join("agents/dev.md").display()
        ),
    );
    assert!(
        !s.root.join("out/argv").exists(),
        "a clone holding the project's id shares its agents, and nothing started"
    );
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
            "--name=repo.dev",
            "--append-system-prompt",
            "Keep it short.",
            "--model",
            "haiku"
        ],
        "a session in the worktree takes the project's name, not the worktree's"
    );
    assert_eq!(s.claude_dir(), wt);
    ok(
        s.cmd(&wt).args(["session-name", "dev"]).output().unwrap(),
        "repo.dev\n",
    );
    let t = s.store().thread("dev").unwrap();
    let b = t
        .snapshot()
        .unwrap()
        .binding("claude", &session)
        .unwrap()
        .unwrap();
    let transcript = s.project(&wt).join(format!("{session}.jsonl"));
    let from_home = transcript.strip_prefix(s.root.join("claude")).unwrap();
    assert!(
        b.transcript
            .as_deref()
            .is_some_and(|p| p.ends_with(from_home)),
        "Claude Code keeps the session under the worktree: {:?}",
        b.transcript
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
            "--name=repo.dev",
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
    assert_eq!(
        argv[..4],
        [
            "--resume",
            &session,
            "--name=repo.dev",
            "--append-system-prompt"
        ]
    );
    assert_eq!(
        argv[4..].join("\n"),
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
    assert_eq!(s.argv(), ["--resume", "s-2", "--name=other.dev"]);
    assert_eq!(s.claude_dir(), other);
}

#[test]
fn build_writes_rewrites_and_previews_the_lines() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(3, |k| format!("question {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    s.line("dev", 1, 1, "first, by hand");
    let fresh = "0|user: question 0 ai: reply 0\n";
    let o = s.ai_in("s-1", &["build", "--preview", "--model", "opus"]);
    let shown = stdout(&o);
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""));
    assert!(
        !shown.contains(fresh),
        "the line written by hand stays: {shown}"
    );
    assert!(
        shown.ends_with("Preview: 4 lines, 0 by the model; nothing stored.\n"),
        "{shown}"
    );
    let all = |s: &Sandbox| {
        let t = s.store().thread("dev").unwrap();
        t.snapshot().unwrap().all_lines().unwrap().len()
    };
    assert_eq!(all(&s), 1, "a preview stores nothing");
    let log = fs::read_to_string(s.store().thread("dev").unwrap().log_path()).unwrap();
    assert!(
        log.contains("preview{pid=") && log.contains("model=\"opus\""),
        "{log}"
    );

    build_ends(
        s.ai_in("s-1", &["build"]),
        "Built 4 lines: 0 by the model, 4 joined.\n",
    );
    ok(s.ai(&["build", "-a", "dev"]), "Nothing to write.\n");
    let o = s.ai_in("s-1", &["build", "--rebuild", "--preview"]);
    let shown = stdout(&o);
    assert!(
        shown.starts_with(&format!(
            "0.1|ai: reply 0\n  was: ai: reply 0\n{fresh}  was: first, by hand\n"
        )),
        "{shown}"
    );
    assert!(shown.ends_with("Preview: 5 lines, 0 by the model; nothing stored.\n"));
    build_ends(
        s.ai_in("s-1", &["build", "--rebuild"]),
        "Built 5 lines: 0 by the model, 5 joined. Dropped 5 lines first.\n",
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
fn a_build_that_finds_another_at_work_runs_no_key_s_command() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(3, |k| format!("question {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let ran = s.root.join("ran");
    fs::write(
        s.root.join("home/.ai/ai.toml"),
        format!(
            "[chat]\nbackground = false\n\n[summariser]\nbackend = \"api\"\nmodel = \"heabsy/qwen38\"\n\n[summariser.keys]\nheabsy = \"touch '{}'; exit 1\"\n",
            ran.display()
        ),
    )
    .unwrap();
    let lock = s.store().thread("dev").unwrap().try_lock().unwrap();
    assert!(lock.is_some());
    let r = s.ai_in("s-1", &["build"]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(
        stdout(&r).contains("A summariser is already running on dev; it writes these lines too."),
        "{}",
        stdout(&r)
    );
    assert!(!ran.exists(), "the key's command ran behind another build");
    drop(lock);

    let r = s.ai_in("s-1", &["build"]);
    assert_eq!(code(&r), 1, "{}", stdout(&r));
    assert!(
        stderr(&r).contains("` failed: exit status: 1."),
        "{}",
        stderr(&r)
    );
    assert!(ran.exists());
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
    build_ends(
        s.ai_in("s-1", &["build", "0+2"]),
        "Built 3 lines: 0 by the model, 3 joined.\n",
    );
    ok(
        s.ai_in("s-1", &["build", "0"]),
        "0 is written already. Rewrite it with --rebuild.\n",
    );
    ok(
        s.ai_in("s-1", &["build", "0", "--rebuild", "--preview"]),
        "0.1|ai: reply 0\n0|user: question 0 ai: reply 0\n  was: by hand\n\
         Preview: 2 lines, 0 by the model; nothing stored.\n",
    );
    let shown = stdout(&s.ai(&["build", "-a", "dev", "2+2", "--preview"]));
    let ids: Vec<&str> = shown
        .lines()
        .map(|l| l.split('|').next().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "2.1",
            "2",
            "3.1",
            "3",
            "2+2",
            "Preview: 5 lines, 0 by the model; nothing stored."
        ],
        "{shown}"
    );
    build_ends(
        s.ai_in("s-1", &["build"]),
        "Built 7 lines: 0 by the model, 7 joined.\n",
    );
    build_ends(
        s.ai_in("s-1", &["build", "0", "--rebuild"]),
        "Built 1 line: 0 by the model, 1 joined. Dropped 3 lines first.\n\
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
    build_ends(
        s.ai_in("s-1", &["build"]),
        "Built 2 lines: 0 by the model, 2 joined.\n",
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

/// The plan `o` printed, up to its log line, after a run that succeeded and printed nothing
/// else.
fn plan_of(o: &Output) -> String {
    assert_eq!((code(o), stderr(o).as_str()), (0, ""), "{}", stdout(o));
    let out = stdout(o);
    let (plan, log) = out.rsplit_once("Log: ").expect("a log line");
    assert!(
        log.ends_with(".log\n") && !log.trim_end().contains('\n'),
        "{out}"
    );
    plan.to_string()
}

#[test]
fn a_dry_run_prints_the_plan_and_calls_no_model() {
    let s = Sandbox::new();
    s.fake_compactor("0");
    s.transcript("s-1", &talk(3, |k| long("ok", k)));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let table = |written: [u64; 3], left: [u64; 3]| {
        format!(
            "  level           lines  written  to write\n\
             \x20 L0  work            2  {:>7}  {:>8}\n\
             \x20 L1  1 turn          2  {:>7}  {:>8}\n\
             \x20 L2  2 turns         1  {:>7}  {:>8}\n",
            written[0], left[0], written[1], left[1], written[2], left[2]
        )
    };
    assert_eq!(
        plan_of(&s.ai_in("s-1", &["build", "--dry-run"])),
        format!(
            "Building agent dev with sonnet, 8 calls at once.\n{}\
             Up to 5 model calls: a line whose sources fit in one is joined without a call.\n\
             No past calls to estimate cost and time from.\n",
            table([0; 3], [2, 2, 1])
        )
    );
    assert!(
        plan_of(&s.ai_in("s-1", &["build", "--dry-run", "--model", "opus"]))
            .starts_with("Building agent dev with opus, 8 calls at once.\n")
    );
    assert_eq!(s.calls(), 0, "a dry run calls no model");
    let lines = |s: &Sandbox| {
        let t = s.store().thread("dev").unwrap();
        t.snapshot().unwrap().all_lines().unwrap().len()
    };
    assert_eq!(lines(&s), 0, "a dry run stores nothing");

    build_ends(
        s.ai_in("s-1", &["build"]),
        "Built 5 lines: 2 by the model, 3 joined.\n",
    );
    let calls = s.calls();
    ok(
        s.ai_in("s-1", &["build", "--dry-run"]),
        "Nothing to write.\n",
    );
    ok(
        s.ai_in("s-1", &["build", "0", "--dry-run"]),
        "0 is written already. Rewrite it with --rebuild.\n",
    );
    let rewrite = plan_of(&s.ai_in("s-1", &["build", "0", "--rebuild", "--dry-run"]));
    assert!(
        rewrite.starts_with(&format!(
            "Rewriting line 0 of agent dev with sonnet, 8 calls at once.\n{}\
             Up to 1 model call:",
            table([2, 1, 0], [0, 1, 0])
        )),
        "{rewrite}"
    );
    assert!(
        rewrite.ends_with("at the average of the last 2 calls.\n"),
        "past calls give the estimate: {rewrite}"
    );
    fs::remove_file(s.root.join("bin/claude")).unwrap();
    let rebuild = plan_of(&s.ai_in("s-1", &["build", "--rebuild", "--dry-run"]));
    assert!(
        rebuild.starts_with(&format!(
            "Rebuilding agent dev with sonnet, 8 calls at once.\n{}",
            table([0; 3], [2, 2, 1])
        )),
        "a dry run needs no claude: {rebuild}"
    );
    assert_eq!(s.calls(), calls, "no dry run called the model");
    assert_eq!(lines(&s), 5, "no dry run dropped a line");
}

#[test]
fn dry_run_and_preview_cannot_be_used_together() {
    let s = Sandbox::new();
    let o = s.ai(&["build", "-a", "dev", "--dry-run", "--preview"]);
    assert_eq!(code(&o), 2, "{}", stderr(&o));
    assert!(
        stderr(&o).contains("'--dry-run' cannot be used with '--preview'"),
        "{}",
        stderr(&o)
    );
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
    let log = s.store().thread("dev").unwrap().log_path();
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
fn build_off_a_terminal_prints_its_plan_each_line_and_its_end() {
    let s = Sandbox::new();
    s.fake_compactor("0.2");
    s.transcript("s-1", &talk(3, |k| long("ok", k)));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let o = s.ai_in("s-1", &["build"]);
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""), "{}", stdout(&o));
    let log = s.store().thread("dev").unwrap().log_path();
    let log = regex::escape(&format!(
        "~/{}",
        log.strip_prefix(s.root.join("home")).unwrap().display()
    ));
    let want = format!(
        "^Building agent dev with sonnet, 8 calls at once\\.
  level           lines  written  to write
  L0  work            2        0         2
  L1  1 turn          2        0         2
  L2  2 turns         1        0         1
Up to 5 model calls: a line whose sources fit in one is joined without a call\\.
No past calls to estimate cost and time from\\.
Log: {log}
\\[1/5\\]  joined 0\\.1
\\[2/5\\]  joined 1\\.1
\\[3/5\\]   built [01]          \\d+\\.\\ds  6 chars  \\$0\\.0000
\\[4/5\\]   built [01]          \\d+\\.\\ds  6 chars  \\$0\\.0000
\\[5/5\\]  joined 0\\+2
Built 5 lines in \\d+s: 2 by the model, 3 joined\\.
2 calls \\(0 to shorten a line, 0 after a failure\\), \\$0\\.0000, 0 tokens\\.
Log: {log}
$"
    );
    assert!(
        Regex::new(&want).unwrap().is_match(&stdout(&o)),
        "{}",
        stdout(&o)
    );
    let past = s.ai_in("s-1", &["build", "--rebuild"]);
    assert!(
        stdout(&past).contains("\nAbout $0.0000 and 0s, at the average of the last 2 calls.\n"),
        "{}",
        stdout(&past)
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
        stdout(&o)
            .ends_with("Failed 0: 600 characters after 5 tries; this line holds at most 512\n"),
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

/// Whether process `pid` runs, a zombie not counted.
fn running(pid: i32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|s| s.split_whitespace().nth(2).is_none_or(|state| state != "Z"))
}

/// Waits for `child` up to `secs` seconds; its exit code.
fn exits_within(mut child: std::process::Child, secs: u64) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code().unwrap_or(-1);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("it still ran after {secs} s");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_killed_compact_session_stops_its_calls_and_the_next_goes_on() {
    let s = Sandbox::new();
    s.fake_compactor("0");
    let tag = |k| if k == 1 { "SLOW" } else { "ok" };
    s.transcript("s-1", &talk(21, |k| long(tag(k), k)));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let ends = turn_ends(&s, "dev");
    let (hang, pid) = (s.root.join("out/hang"), s.root.join("out/slow.pid"));
    fs::write(&hang, "").unwrap();
    let compact = || {
        s.cmd(&s.repo())
            .args(["compact-session", "--session", "s-1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let child = compact();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !(pid.exists() && s.has_line("dev", 1, ends[0])) {
        assert!(Instant::now() < deadline, "the slow call never started");
        thread::sleep(Duration::from_millis(20));
    }
    let slow: i32 = fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
    assert!(running(slow));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    assert_eq!(exits_within(child, 5), -1, "ended by the signal");
    let deadline = Instant::now() + Duration::from_secs(5);
    while running(slow) {
        assert!(
            Instant::now() < deadline,
            "the call outlived compact-session"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(s.has_line("dev", 1, ends[0]), "a written line stays");
    assert!(!s.has_line("dev", 1, ends[1]));
    fs::remove_file(&hang).unwrap();
    assert_eq!(
        exits_within(compact(), 30),
        0,
        "the next compaction goes on"
    );
    assert!(s.has_line("dev", 1, ends[1]));
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
fn the_users_repository_holds_the_threads_committed_on_events() {
    let s = Sandbox::new();
    let ai = s.homes().repo;
    let folder = ai.join("projects/repo");
    let threads = folder.join("agents/threads");
    let id = Project::find(&s.repo()).unwrap().unwrap().id;
    assert_eq!(
        fs::read_to_string(folder.join("id")).unwrap(),
        format!("{id}\n"),
        "ai init claims the project's folder"
    );
    assert_eq!(
        fs::read_link(s.repo().join("ai.local.toml")).unwrap(),
        folder.join("ai.toml")
    );
    ok(
        s.ai(&["list"]),
        "No agents in this project. Create one: ai agent new <name>\n",
    );
    assert!(!threads.exists(), "a read makes no thread");
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    assert!(
        !s.repo().join(".ai").exists(),
        "nothing of ai in the project"
    );
    let log = || git(&ai, &["log", "--format=%an: %s"]);
    assert_eq!(log(), "ai: ai agent new dev\n");

    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    assert_eq!(
        log(),
        "ai: ai agent attach dev\nai: ai agent new dev\n",
        "a write that changes nothing makes no commit"
    );
    let files = git(&ai, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        files.contains("\nprojects/repo/agents/threads/dev/items.jsonl\n"),
        "{files}"
    );
    let sources = fs::read_to_string(threads.join("dev/sources.jsonl")).unwrap();
    let transcript = s.project(&s.repo()).join("s-1.jsonl");
    assert!(
        sources.contains(&format!("\"path\":\"{}\"", transcript.display())),
        "a transcript is recorded where it really sits: {sources}"
    );
    assert!(
        s.homes()
            .machine
            .join("agents")
            .join(&id)
            .join("threads/dev/write.lock")
            .is_file()
    );
    assert!(!files.contains(".lock"), "{files}");
    assert_eq!(git(&ai, &["status", "--porcelain"]), "");
}

#[test]
fn a_projects_name_held_by_another_project_is_refused() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    let other = s.root.join("elsewhere/repo");
    fs::create_dir_all(&other).unwrap();
    let o = s.bin(&other).arg("init").output().unwrap();
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    let id = Project::find(&s.repo()).unwrap().unwrap().id;
    assert_eq!(
        stderr(&o),
        format!(
            "Another project ({id}) holds the name repo in ~/ai/projects, so the project at {} \
             can't take it. Give this one another name: set [project] name in {}.\n",
            other.display(),
            other.join("ai.toml").display()
        )
    );
    assert!(
        !other.join("ai.toml").exists(),
        "a refused init mints no id"
    );
    fs::write(other.join("ai.toml"), "[project]\n").unwrap();
    fs::write(
        other.join("ai.toml"),
        format!(
            "{}name = \"repo-2\"\n",
            fs::read_to_string(other.join("ai.toml")).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(code(&s.bin(&other).arg("init").output().unwrap()), 0);
    assert!(s.homes().repo.join("projects/repo-2/id").is_file());
}

#[test]
fn a_project_whose_name_another_holds_reads_its_settings_and_only_its_agents_fail() {
    let s = Sandbox::new();
    assert_eq!(code(&s.top(None, &["memory", "init"])), 0);
    let held = Project::find(&s.repo()).unwrap().unwrap().id;
    let other = s.root.join("elsewhere/repo");
    fs::create_dir_all(&other).unwrap();
    let id = "project_01h455vb4pex5vsknk084sn02q";
    fs::write(other.join("ai.toml"), format!("[project]\nid = \"{id}\"\n")).unwrap();

    let note = s
        .bin(&other)
        .args(["memory", "note", "a fact"])
        .output()
        .unwrap();
    assert_eq!(code(&note), 0, "{}", stderr(&note));
    let view = s.bin(&other).args(["memory", "view"]).output().unwrap();
    assert_eq!(code(&view), 0, "{}", stderr(&view));
    assert!(stdout(&view).contains("a fact"), "{}", stdout(&view));

    fails(
        s.cmd(&other).arg("list").output().unwrap(),
        &format!(
            "Another project ({held}) holds the name repo in ~/ai/projects, so this project \
             ({id}, at {}) can't take it. Give this one another name: set [project] name in {}.\n",
            other.display(),
            other.join("ai.toml").display()
        ),
    );
}

#[test]
fn a_turns_sync_waits_for_the_next_event_to_be_committed() {
    let s = Sandbox::new();
    s.fake_compactor("0");
    assert_eq!(code(&s.top(None, &["memory", "init"])), 0);
    let ai = s.homes().repo;
    let log = || git(&ai, &["log", "--format=%s"]);
    let clean = || git(&ai, &["status", "--porcelain"]).is_empty();
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
    let ai = s.homes().repo;
    assert_eq!(git(&ai, &["log", "--format=%s"]), "ai agent new dev\n");
    assert!(ai.join("projects/repo/agents/threads/dev").is_dir());
    assert_eq!(git(&s.repo(), &["status", "--porcelain"]), "");
}

#[test]
fn a_store_ai_0_17_kept_is_refused_naming_the_converter() {
    let s = Sandbox::new();
    // As ai 0.17 left it: no folder of the project in ~/ai.
    fs::remove_dir_all(s.homes().repo.join("projects/repo")).unwrap();
    let agents = s.repo().join(".ai/local/agents");
    fs::create_dir_all(agents.join("threads/dev")).unwrap();
    fs::write(agents.join("format"), "4\n").unwrap();
    let refused = format!(
        "This project's agents and private settings are still in {}, where ai 0.17 kept them. \
         Convert them to ~/ai/projects with `ai convert {}`.\n",
        s.repo().join(".ai/local").display(),
        s.repo().display(),
    );
    fails(s.ai(&["list"]), &refused);
    fails(s.ai(&["new", "other"]), &refused);
    assert!(!s.homes().repo.join("projects/repo/agents").exists());
}

#[test]
fn ai_convert_moves_what_ai_0_17_kept_to_the_project_s_folder() {
    let s = Sandbox::new();
    // As ai 0.17 left it: no folder of the project in ~/ai, private settings in .ai/local.
    let folder = s.homes().repo.join("projects/repo");
    fs::remove_dir_all(&folder).unwrap();
    let local = s.repo().join(".ai/local");
    fs::create_dir_all(&local).unwrap();
    let settings = "[summariser]\nmodel = \"opus\"\n";
    fs::write(local.join("ai.toml"), settings).unwrap();
    let said = |verb: &str| {
        format!(
            "repo: {verb} private settings from {} to \
             ~/ai/projects/repo.\n",
            local.display()
        )
    };

    ok(
        s.top(None, &["convert", "--dry-run"]),
        &said("would convert"),
    );
    assert!(!folder.exists());

    ok(
        s.top(None, &["convert"]),
        &format!(
            "{}  {} is unchanged; its agents are no longer read, and once you have checked the \
             conversion, are yours to delete.\n",
            said("converted"),
            local.display()
        ),
    );
    assert_eq!(
        fs::read_to_string(folder.join("ai.toml")).unwrap(),
        settings
    );
    assert_eq!(fs::read_to_string(local.join("ai.toml")).unwrap(), settings);
    ok(
        s.ai(&["list"]),
        "No agents in this project. Create one: ai agent new <name>\n",
    );

    ok(
        s.top(None, &["convert"]),
        "repo: already converted to ~/ai/projects/repo.\n",
    );
}

#[test]
fn ai_convert_over_several_directories_tells_of_each_project_and_counts_the_rest() {
    let s = Sandbox::new();
    fs::remove_dir_all(s.homes().repo.join("projects/repo")).unwrap();
    let local = s.repo().join(".ai/local");
    fs::create_dir_all(&local).unwrap();
    fs::write(local.join("ai.toml"), "[summariser]\nmodel = \"opus\"\n").unwrap();
    let empty = s.root.join("empty");
    s.init(&empty);
    let plain = s.root.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let out = s.root.join("out");
    let arg = |p: &Path| p.display().to_string();

    ok(
        s.top(
            None,
            &[
                "convert",
                "--dry-run",
                &arg(&s.repo()),
                &arg(&plain),
                &arg(&empty),
                &arg(&out),
            ],
        ),
        &format!(
            "repo: would convert private settings from {} to \
             ~/ai/projects/repo.\nSkipped 2 directories that hold no ai project, and 1 project \
             with nothing to convert in .ai/local.\n",
            local.display()
        ),
    );
    ok(
        s.top(None, &["convert", "--dry-run", &arg(&plain), &arg(&empty)]),
        "Skipped 1 directory that holds no ai project, and 1 project with nothing to convert in \
         .ai/local.\n",
    );
}

#[test]
fn ai_convert_given_one_directory_says_what_it_found_there() {
    let s = Sandbox::new();
    let empty = s.root.join("empty");
    s.init(&empty);
    let plain = s.root.join("plain");
    fs::create_dir_all(&plain).unwrap();

    ok(
        s.top(None, &["convert", &plain.display().to_string()]),
        &format!(
            "{}: not a project (no ai.toml with a project id), nothing to convert.\n",
            plain.display()
        ),
    );
    ok(
        s.top(None, &["convert", &empty.display().to_string()]),
        "empty: .ai/local holds no agents and no private settings, nothing to convert.\n",
    );
    ok(
        s.top(None, &["convert"]),
        "repo: .ai/local holds no agents and no private settings, nothing to convert.\n",
    );
}

/// Sets `[agent] gc-after-mib` in the user's `ai.toml`; `None` leaves it unset.
fn gc_after(s: &Sandbox, mib: Option<u64>) {
    let key = mib.map_or(String::new(), |m| {
        format!("\n[agent]\ngc-after-mib = {m}\n")
    });
    fs::write(
        s.root.join("home/.ai/ai.toml"),
        format!("[chat]\nbackground = false\n{key}"),
    )
    .unwrap();
}

/// `ai agent args` in the repository with no `git` on `PATH`.
fn without_git(s: &Sandbox, args: &[&str]) -> Output {
    s.cmd(&s.repo())
        .env("PATH", s.root.join("bin"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn a_commit_past_gc_after_mib_starts_git_gc_on_the_repository() {
    let s = Sandbox::new();
    gc_after(&s, Some(0));
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    let git_dir = s.homes().repo.join(".git");
    let packed = || {
        fs::read_dir(git_dir.join("objects/pack")).is_ok_and(|d| {
            d.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "pack"))
        }) && !git_dir.join("gc.pid").exists()
    };
    let mut waited = 0;
    while !packed() {
        assert!(waited < 600, "git gc packed nothing in a minute");
        std::thread::sleep(std::time::Duration::from_millis(100));
        waited += 1;
    }
    assert_eq!(repo::loose_bytes(&s.homes().repo).unwrap(), 0);
    git(&s.homes().repo, &["fsck", "--strict"]);
}

#[test]
fn gc_after_mib_decides_whether_a_commit_needs_git() {
    let s = Sandbox::new();
    gc_after(&s, None);
    ok(
        without_git(&s, &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    gc_after(&s, Some(1));
    ok(
        without_git(&s, &["new", "other"]),
        "Created agent other. Start it: ai agent resume other\n",
    );
    gc_after(&s, Some(0));
    let o = without_git(&s, &["new", "third"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "Created agent third. Start it: ai agent resume third\n"
    );
    assert_eq!(
        stderr(&o),
        "Cannot pack ~/ai: no git program on PATH. Install git: ai runs git gc on it once more \
         than [agent] gc-after-mib of loose objects have been written since its last one.\n"
    );
    let status = git(&s.homes().repo, &["status", "--porcelain"]);
    assert_eq!(status, "", "the commit stands");
}

#[test]
fn rename_moves_an_agent_and_keeps_its_sessions_attached() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    ok(
        s.ai_in("s-1", &["rename", "dev", "reviewer"]),
        "Renamed agent dev to reviewer.\n",
    );
    ok(
        s.ai_in("s-1", &["list"]),
        "reviewer  2 turns  2026-10-14 10:00\n\
         \x20 claude  s-1  attached  2026-10-14 10:00  this session\n",
    );
    s.transcript("s-1", &talk(3, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["sync-session"]), "");
    assert_eq!(turn_ends(&s, "reviewer").len(), 3);

    let ai = s.homes().repo;
    assert_eq!(
        git(&ai, &["log", "-1", "--format=%s"]),
        "ai agent rename dev reviewer\n"
    );
    let tree = git(&ai, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(
        tree.lines()
            .any(|l| l == "projects/repo/agents/threads/reviewer/items.jsonl"),
        "{tree}"
    );
    assert!(!tree.contains("/threads/dev/"), "{tree}");
    fails(
        s.ai(&["view", "dev"]),
        "No agent dev in this project. Create it: ai agent new dev\n",
    );
}

#[test]
fn rename_refuses_a_missing_agent_a_taken_name_and_a_bad_one() {
    let s = Sandbox::new();
    for name in ["dev", "two"] {
        ok(
            s.ai(&["new", name]),
            &format!("Created agent {name}. Start it: ai agent resume {name}\n"),
        );
    }
    let ai = s.homes().repo;
    let before = git(&ai, &["log", "--format=%H"]);
    fails(
        s.ai(&["rename", "nope", "x"]),
        "No agent nope in this project. Create it: ai agent new nope\n",
    );
    fails(
        s.ai(&["rename", "dev", "two"]),
        "Agent two already exists in this project.\n",
    );
    fails(
        s.ai(&["rename", "dev", "dev"]),
        "Agent dev is already called dev.\n",
    );
    fails(
        s.ai(&["rename", "dev", "a/b"]),
        "'a/b' is not a thread name: letters, digits, '-' and '_'.\n",
    );
    let o = s.outside(&["rename", "dev", "x"]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).starts_with("Not in a project"), "{}", stderr(&o));
    assert_eq!(git(&ai, &["log", "--format=%H"]), before);
    let list = stdout(&s.ai(&["list"]));
    assert!(
        list.contains("dev  0 turns") && list.contains("two  0 turns"),
        "{list}"
    );
}

#[test]
fn rename_names_each_setting_that_still_names_the_agent_and_changes_none() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    let user = s.root.join("home/.ai/ai.toml");
    let project = s.repo().join("ai.toml");
    let private = s.homes().repo.join("projects/repo/ai.toml");
    let append = |p: &Path, text: &str| {
        let mut f = fs::OpenOptions::new().append(true).open(p).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    };
    append(&user, "\n[agents.dev]\ninstructions = \"dev.md\"\n");
    append(
        &project,
        "\n[agents.dev]\nworktree = \"../dev\"\n\n[agents.lead]\nreports-to = [\"dev\"]\n",
    );
    fs::write(&private, "[agents.qa]\nreports-to = [\"main\", \"dev\"]\n").unwrap();
    let texts: Vec<String> = [&user, &project, &private]
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    let line_of =
        |text: &str, needle: &str| text.lines().position(|l| l.contains(needle)).unwrap() + 1;

    let o = s.ai(&["rename", "dev", "reviewer"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(stdout(&o), "Renamed agent dev to reviewer.\n");
    let project_at = project.display();
    assert_eq!(
        stderr(&o),
        format!(
            "~/.ai/ai.toml:{}: [agents.dev] no longer applies here: reviewer reads [agents.reviewer].\n\
             {project_at}:{}: [agents.dev] no longer applies here: reviewer reads [agents.reviewer].\n\
             {project_at}:{}: lead's reports-to names dev, now reviewer.\n\
             ~/ai/projects/repo/ai.toml:2: qa's reports-to names dev, now reviewer.\n",
            line_of(&texts[0], "[agents.dev]"),
            line_of(&texts[1], "[agents.dev]"),
            line_of(&texts[1], "reports-to"),
        )
    );
    for (p, text) in [&user, &project, &private].iter().zip(&texts) {
        assert_eq!(&fs::read_to_string(p).unwrap(), text, "{}", p.display());
    }
}

#[test]
fn rename_goes_ahead_when_a_settings_file_cannot_be_read() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    fs::write(s.root.join("home/.ai/ai.toml"), "[memroy]\n").unwrap();
    let o = s.ai(&["rename", "dev", "reviewer"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(stdout(&o), "Renamed agent dev to reviewer.\n");
    let err = stderr(&o);
    assert!(
        err.starts_with("Cannot check the settings for dev: ~/.ai/ai.toml: ")
            && err.contains("memroy"),
        "{err}"
    );
    assert!(s.store().thread("reviewer").unwrap().exists().unwrap());
}

/// Runs the restore command `ai agent remove` printed in `o`.
fn restore(s: &Sandbox, o: &Output) -> Output {
    let out = stdout(o);
    let cmd = out
        .trim_end()
        .split_once("Restore it: ai agent ")
        .unwrap()
        .1;
    s.ai(&cmd.split(' ').collect::<Vec<_>>())
}

#[test]
fn remove_takes_an_agent_out_and_its_restore_command_brings_it_back() {
    let s = Sandbox::new();
    for name in ["dev", "two"] {
        ok(
            s.ai(&["new", name]),
            &format!("Created agent {name}. Start it: ai agent resume {name}\n"),
        );
    }
    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let transcript = s.transcript("s-1", &talk(3, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["sync-session"]), "");
    assert_eq!(turn_ends(&s, "dev").len(), 3);
    let bytes = fs::read(&transcript).unwrap();

    let o = s.ai(&["remove", "dev"]);
    assert_eq!((code(&o), stderr(&o).as_str()), (0, ""));
    let ai = s.homes().repo;
    let kept = git(&ai, &["rev-parse", "HEAD~1"]);
    assert_eq!(
        stdout(&o),
        format!(
            "Removed agent dev. Restore it: ai agent restore dev --commit {}\n",
            kept.trim_end()
        )
    );
    assert_eq!(
        git(&ai, &["log", "-2", "--format=%s"]),
        "ai agent remove dev\nai agent remove dev: its last state\n"
    );
    let tree = git(&ai, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(!tree.contains("/threads/dev/"), "{tree}");
    ok(s.ai(&["list"]), "two  0 turns  -\n");
    unattached(s.ai_in("s-1", &["sync-session"]));
    assert_eq!(
        fs::read(&transcript).unwrap(),
        bytes,
        "the transcript stays"
    );

    ok(restore(&s, &o), "Restored agent dev.\n");
    assert_eq!(
        git(&ai, &["log", "-1", "--format=%s"]),
        "ai agent restore dev\n"
    );
    ok(s.ai_in("s-1", &["sync-session"]), "");
    assert_eq!(turn_ends(&s, "dev").len(), 3);
    let list = stdout(&s.ai_in("s-1", &["list"]));
    assert!(
        list.starts_with("dev  3 turns  2026-10-14 10:00\n")
            && list.contains("claude  s-1  attached  2026-10-14 10:00  this session"),
        "{list}"
    );
}

#[test]
fn remove_refuses_while_an_attached_session_runs() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    s.transcript("s-2", &talk(1, |k| format!("ask {k}")));
    ok(s.ai_in("s-2", &["attach", "dev"]), "Attached to dev.\n");
    let ai = s.homes().repo;
    let before = git(&ai, &["log", "--format=%H"]);
    let agents = s.root.join("out/agents.json");

    fs::write(
        &agents,
        r#"[{"cwd":"/r","kind":"interactive","startedAt":1,"pid":1,"status":"idle","sessionId":"s-1","name":"repo.dev"},
            {"cwd":"/r","kind":"interactive","startedAt":2,"pid":2,"status":"busy","sessionId":"s-9"}]"#,
    )
    .unwrap();
    fails(
        s.ai(&["remove", "dev"]),
        "Agent dev's session claude s-1 (repo.dev) is running. End it, then try again.\n",
    );
    fs::write(
        &agents,
        r#"[{"kind":"background","state":"working","sessionId":"s-1"},
            {"kind":"interactive","pid":3,"sessionId":"s-2"}]"#,
    )
    .unwrap();
    fails(
        s.ai(&["remove", "dev"]),
        "Agent dev's sessions claude s-1, claude s-2 are running. End them, then try again.\n",
    );
    fs::write(s.root.join("out/agents.fail"), "").unwrap();
    fails(
        s.ai(&["remove", "dev"]),
        "Cannot tell whether agent dev's sessions are running: claude agents --json failed: \
         boom.\n",
    );
    assert_eq!(git(&ai, &["log", "--format=%H"]), before);
    assert!(stdout(&s.ai(&["list"])).starts_with("dev  "));

    fs::remove_file(s.root.join("out/agents.fail")).unwrap();
    ok(s.ai_in("s-2", &["detach"]), "Detached from dev.\n");
    fs::write(&agents, r#"[{"sessionId":"s-2"}]"#).unwrap();
    let o = s.ai(&["remove", "dev"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).starts_with("Removed agent dev. Restore it: "));
}

#[test]
fn remove_of_an_agent_that_holds_nothing_says_how_to_make_it_again() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(
        s.ai(&["remove", "dev"]),
        "Removed agent dev, which held nothing. Make it again: ai agent new dev\n",
    );
    fails(
        s.ai(&["remove", "dev"]),
        "No agent dev in this project. Create it: ai agent new dev\n",
    );
    let o = s.outside(&["remove", "dev"]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).starts_with("Not in a project"), "{}", stderr(&o));
}

#[test]
fn remove_names_each_setting_that_still_names_the_agent_and_changes_none() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    let project = s.repo().join("ai.toml");
    let mut f = fs::OpenOptions::new().append(true).open(&project).unwrap();
    f.write_all(
        b"\n[agents.dev]\nworktree = \"../dev\"\n\n[agents.lead]\nreports-to = [\"dev\"]\n",
    )
    .unwrap();
    let text = fs::read_to_string(&project).unwrap();
    let line_of = |needle: &str| text.lines().position(|l| l.contains(needle)).unwrap() + 1;

    let o = s.ai(&["remove", "dev"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "Removed agent dev, which held nothing. Make it again: ai agent new dev\n"
    );
    let at = project.display();
    assert_eq!(
        stderr(&o),
        format!(
            "{at}:{}: [agents.dev] applies to no agent now.\n\
             {at}:{}: lead's reports-to names dev, which was removed.\n",
            line_of("[agents.dev]"),
            line_of("reports-to"),
        )
    );
    assert_eq!(fs::read_to_string(&project).unwrap(), text);
}

#[test]
fn rename_waits_for_a_running_summariser() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    let dev = s.store().thread("dev").unwrap();
    let summarising = dev.lock().unwrap();
    let mut child = s
        .cmd(&s.repo())
        .args(["rename", "dev", "reviewer"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    io::BufRead::read_line(
        &mut io::BufReader::new(child.stderr.take().unwrap()),
        &mut line,
    )
    .unwrap();
    assert_eq!(line, "Waiting for agent dev's summariser to finish.\n");
    assert!(dev.exists().unwrap());
    drop(summarising);
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 0);
    assert_eq!(stdout(&o), "Renamed agent dev to reviewer.\n");
    assert!(!dev.exists().unwrap());
}

#[test]
fn list_tells_the_files_a_session_read_that_no_longer_hold_it() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(
        s.ai_in("s-1", &["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let line = |session: &str| {
        let o = s.ai_in(session, &["list"]);
        assert_eq!((code(&o), stderr(&o).as_str()), (0, ""));
        stdout(&o)
            .lines()
            .find(|l| l.contains(&format!("  {session}  ")))
            .unwrap()
            .to_owned()
    };
    assert!(line("s-1").ends_with("this session"), "{}", line("s-1"));

    let gone = s.transcript("s-2", &talk(1, |_| "brief".into()));
    ok(s.ai_in("s-2", &["attach", "dev"]), "Attached to dev.\n");
    fs::remove_file(&gone).unwrap();
    let o = s.ai_in("s-2", &["sync-session"]);
    assert_eq!(code(&o), 0);
    assert!(
        stderr(&o).starts_with(&format!(
            "Thread dev: {} is gone; this thread read ",
            gone.display()
        )) && stderr(&o)
            .ends_with(" of it. `ai backup threads` tells whether the backup holds them.\n"),
        "{}",
        stderr(&o)
    );
    assert!(
        line("s-2").ends_with("this session  1 file changed or gone"),
        "{}",
        line("s-2")
    );
    assert!(line("s-1").ends_with("this session"), "{}", line("s-1"));
    assert!(
        stdout(&s.ai(&["list"])).ends_with(
            "Files changed or gone: `ai backup threads` tells whether the backup holds what was \
             read of them.\n"
        ),
        "{}",
        stdout(&s.ai(&["list"]))
    );
}

#[test]
fn list_goes_on_past_a_thread_whose_source_records_cannot_be_read() {
    let s = Sandbox::new();
    s.transcript("s-1", &talk(1, |_| "ask".into()));
    for name in ["dev", "other"] {
        s.ai(&["new", name]);
    }
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let sources = s.store().dir().join("threads/other/sources.jsonl");
    fs::write(&sources, "{\"harness\":\"claude\",\"hash\":\"abc\"}\n").unwrap();

    let o = s.ai(&["list"]);
    assert_eq!(code(&o), 1, "the listing fails");
    assert!(
        stderr(&o).starts_with("Thread other: source record 1 is corrupt"),
        "{}",
        stderr(&o)
    );
    let out = stdout(&o);
    assert!(
        out.contains("dev  1 turn") && out.contains("other  0 turns"),
        "{out}"
    );
    assert!(out.contains("  claude  s-1  attached"), "{out}");
}

#[test]
fn resume_brings_a_transcript_that_is_gone_back_from_the_backup() {
    let s = Sandbox::new();
    let config = s.root.join("home/.ai/ai.toml");
    let mut settings = fs::read_to_string(&config).unwrap();
    settings.push_str(&format!(
        "[backup]\nfolders = [\"{}\"]\n",
        s.root.join("claude/projects").display()
    ));
    fs::write(&config, settings).unwrap();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    let text = ran_in(&talk(2, |k| format!("ask {k}")), &s.repo());
    let transcript = s.transcript("s-1", &text);
    let artifact = transcript.with_extension("").join("tool-results/out.txt");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, "kept\n").unwrap();
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let o = s.top(None, &["backup", "run"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    // The file grew after the thread read it; what comes back is the file as last backed up.
    let grown = format!("{text}{{\"type\":\"future-event\"}}\n");
    fs::write(&transcript, &grown).unwrap();
    let o = s.top(None, &["backup", "run"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    fs::remove_file(&transcript).unwrap();

    let o = s.ai(&["resume", "dev"]);
    assert_eq!(
        (code(&o), stderr(&o).as_str()),
        (0, "Brought 1 file of session s-1 back from the backup.\n")
    );
    assert_eq!(s.argv(), ["--resume", "s-1", "--name=repo.dev"]);
    assert_eq!(fs::read_to_string(&transcript).unwrap(), grown);
    assert_eq!(fs::read_to_string(&artifact).unwrap(), "kept\n");

    // Bytes no snapshot holds can't come back: a new session starts, and says why.
    s.ai(&["new", "other"]);
    let never = s.transcript("s-2", &ran_in(&talk(1, |_| "later".into()), &s.repo()));
    ok(s.ai_in("s-2", &["attach", "other"]), "Attached to other.\n");
    fs::remove_file(&never).unwrap();
    let o = s.ai(&["resume", "other"]);
    let e = stderr(&o);
    assert!(
        e.starts_with(
            "Cannot resume other: the transcript of session s-2 is gone, and the backup does not \
             hold the "
        ) && e.ends_with(&format!(
            " this thread read of {}. Starting a new session.\n",
            never.display()
        )),
        "{e}"
    );
    assert_eq!(s.argv()[0], "--session-id");
}

#[test]
fn restore_refuses_while_an_agent_has_the_name_and_keeps_that_agent_whole() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let removed = s.ai(&["remove", "dev"]);
    assert_eq!(code(&removed), 0, "{}", stderr(&removed));
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    s.transcript("s-2", &talk(5, |k| format!("later {k}")));
    ok(s.ai_in("s-2", &["attach", "dev"]), "Attached to dev.\n");
    let ai = s.homes().repo;
    let before = git(&ai, &["log", "--format=%H"]);

    fails(
        restore(&s, &removed),
        "Agent dev exists in this project, and restoring would replace it. Rename it first: ai \
         agent rename dev <new name>\n",
    );
    assert_eq!(turn_ends(&s, "dev").len(), 5);
    assert_eq!(git(&ai, &["log", "--format=%H"]), before);
    s.transcript("s-2", &talk(6, |k| format!("later {k}")));
    ok(s.ai_in("s-2", &["sync-session"]), "");
    assert_eq!(turn_ends(&s, "dev").len(), 6);

    ok(
        s.ai(&["rename", "dev", "newer"]),
        "Renamed agent dev to newer.\n",
    );
    ok(restore(&s, &removed), "Restored agent dev.\n");
    assert_eq!(turn_ends(&s, "dev").len(), 2);
    assert_eq!(turn_ends(&s, "newer").len(), 6);
    ok(s.ai_in("s-1", &["sync-session"]), "");
    ok(s.ai_in("s-2", &["sync-session"]), "");
}

#[test]
fn restore_leaves_a_session_attached_elsewhere_where_it_is() {
    let s = Sandbox::new();
    for name in ["dev", "two"] {
        ok(
            s.ai(&["new", name]),
            &format!("Created agent {name}. Start it: ai agent resume {name}\n"),
        );
    }
    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let removed = s.ai(&["remove", "dev"]);
    assert_eq!(code(&removed), 0, "{}", stderr(&removed));
    ok(s.ai_in("s-1", &["attach", "two"]), "Attached to two.\n");

    let o = restore(&s, &removed);
    assert_eq!(
        (code(&o), stdout(&o).as_str(), stderr(&o).as_str()),
        (
            0,
            "Restored agent dev.\n",
            "Session claude s-1 is attached to two now, so dev holds it detached.\n"
        )
    );
    ok(s.ai_in("s-1", &["sync-session"]), "");
    ok(s.ai_in("s-1", &["detach"]), "Detached from two.\n");
}

#[test]
fn detach_clears_a_session_that_two_agents_hold_attached() {
    let s = Sandbox::new();
    for name in ["dev", "two"] {
        ok(
            s.ai(&["new", name]),
            &format!("Created agent {name}. Start it: ai agent resume {name}\n"),
        );
    }
    s.transcript("s-1", &talk(2, |k| format!("ask {k}")));
    ok(s.ai_in("s-1", &["attach", "dev"]), "Attached to dev.\n");
    let threads = s.homes().repo.join("projects/repo/agents/threads");
    fs::copy(
        threads.join("dev/attachments.jsonl"),
        threads.join("two/attachments.jsonl"),
    )
    .unwrap();

    fails(
        s.ai_in("s-1", &["sync-session"]),
        "Session claude s-1 is attached to dev and two. Detach it, which detaches it from each \
         (ai agent detach --harness claude --session s-1), then attach it to one.\n",
    );
    ok(s.ai_in("s-1", &["detach"]), "Detached from dev and two.\n");
    unattached(s.ai_in("s-1", &["sync-session"]));
    ok(s.ai_in("s-1", &["attach", "two"]), "Attached to two.\n");
}

#[test]
fn remove_sees_a_session_that_runs_under_another_configuration_directory() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    let work = s.root.join("work/projects/-repo");
    fs::create_dir_all(&work).unwrap();
    let transcript = work.join("s-1.jsonl");
    fs::write(&transcript, talk(2, |k| format!("ask {k}"))).unwrap();
    let attached = s
        .cmd(&s.repo())
        .env("CLAUDE_CONFIG_DIR", s.root.join("work"))
        .env("CLAUDE_CODE_SESSION_ID", "s-1")
        .args(["attach", "dev", "--transcript"])
        .arg(&transcript)
        .output()
        .unwrap();
    ok(attached, "Attached to dev.\n");
    fs::write(
        s.root.join("out/agents-work.json"),
        r#"[{"kind":"interactive","pid":1,"sessionId":"s-1","name":"repo.dev"}]"#,
    )
    .unwrap();

    fails(
        s.ai(&["remove", "dev"]),
        "Agent dev's session claude s-1 (repo.dev) is running. End it, then try again.\n",
    );
    fs::remove_file(s.root.join("out/agents-work.json")).unwrap();
    let o = s.ai(&["remove", "dev"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
}

#[test]
fn rename_names_each_running_session_that_keeps_its_old_name() {
    let s = Sandbox::new();
    ok(
        s.ai(&["new", "dev"]),
        "Created agent dev. Start it: ai agent resume dev\n",
    );
    for session in ["s-1", "s-2", "s-3", "s-4"] {
        s.transcript(session, &talk(1, |k| format!("ask {k}")));
        ok(s.ai_in(session, &["attach", "dev"]), "Attached to dev.\n");
    }
    fs::write(
        s.root.join("out/agents.json"),
        r#"[{"sessionId":"s-1","name":"repo.dev"},
            {"sessionId":"s-2"},
            {"sessionId":"s-3","name":"repo.reviewer"},
            {"sessionId":"s-9","name":"repo.dev"}]"#,
    )
    .unwrap();
    let o = s.ai(&["rename", "dev", "reviewer"]);
    assert_eq!(
        (code(&o), stdout(&o).as_str(), stderr(&o).as_str()),
        (
            0,
            "Renamed agent dev to reviewer.\n",
            "Session claude s-1 still runs as repo.dev. Name it again there: /rename \
             repo.reviewer\n\
             Session claude s-2 runs without a name. Name it again there: /rename repo.reviewer\n"
        )
    );

    fs::write(s.root.join("out/agents.fail"), "").unwrap();
    let o = s.ai(&["rename", "reviewer", "dev"]);
    assert_eq!(
        (code(&o), stdout(&o).as_str(), stderr(&o).as_str()),
        (
            0,
            "Renamed agent reviewer to dev.\n",
            "Cannot tell whether agent dev's sessions are running: claude agents --json failed: \
             boom. One that runs keeps its old name. Name it again there: /rename repo.dev\n"
        )
    );
}
