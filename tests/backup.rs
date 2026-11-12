//! `ai backup` run against a real restic, on a made-up home in a temporary directory.

use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use ai_tools::{backup::host, file::sha256};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

/// Variables of the tester's own setup that must not reach a command under test.
const ISOLATE: [&str; 4] = [
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDECODE",
    "RESTIC_CACHE_DIR",
];

/// A made-up home with `~/.ai` and Claude Code's folder of transcripts.
struct Machine {
    _tmp: TempDir,
    root: PathBuf,
}

impl Machine {
    fn new() -> Machine {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let m = Machine { _tmp: tmp, root };
        fs::create_dir_all(m.ai()).unwrap();
        fs::create_dir_all(m.claude()).unwrap();
        m
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn ai(&self) -> PathBuf {
        self.home().join(".ai")
    }

    fn claude(&self) -> PathBuf {
        self.home().join(".claude/projects")
    }

    fn repo(&self) -> PathBuf {
        self.ai().join("backup")
    }

    /// The local repository's password, as restic's flag for it.
    fn key(&self) -> String {
        format!("--password-file={}", self.ai().join("backup.key").display())
    }

    /// Writes the user's `ai.toml`.
    fn settings(&self, text: &str) {
        fs::write(self.ai().join("ai.toml"), text).unwrap();
    }

    fn isolate(&self, c: &mut Command) {
        for k in ISOLATE {
            c.env_remove(k);
        }
        c.current_dir(self.home())
            .env("HOME", self.home())
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("AI_MEMORY_SUMMARISE", "0");
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ai"));
        self.isolate(&mut c);
        c.args(args);
        c
    }

    /// `ai backup args`.
    fn backup(&self, args: &[&str]) -> Output {
        self.command(&["backup"]).args(args).output().unwrap()
    }

    /// `restic args` on `repo`, opened with the flag `password`.
    fn restic(&self, repo: &Path, password: &str, args: &[&str]) -> Output {
        let mut c = Command::new("restic");
        self.isolate(&mut c);
        c.arg("--repo").arg(repo).arg(password).args(args);
        c.output().expect("the tests need restic on PATH")
    }

    /// The snapshots of `repo`, oldest first.
    fn snapshots(&self, repo: &Path, password: &str) -> Vec<Value> {
        let o = self.restic(repo, password, &["snapshots", "--json"]);
        serde_json::from_str(&ok(&o)).unwrap()
    }

    /// The bytes the newest snapshot of `repo` holds of the file `path`.
    fn dump(&self, repo: &Path, password: &str, path: &Path) -> Output {
        self.restic(repo, password, &["dump", "latest", path.to_str().unwrap()])
    }

    /// Records, as a thread of `~/ai` does, that thread `thread` of the project `repo` read
    /// `bytes` from the start of the file at `path` on the machine `host`: a version that goes
    /// on from the `from` bytes its record before tells of, or with 0, a new one.
    fn read_by(&self, thread: &str, host: &str, path: &Path, from: usize, bytes: &[u8]) {
        let record = serde_json::json!({
            "harness": "claude",
            "session": "s-1",
            "host": host,
            "path": path,
            "from": from,
            "len": bytes.len(),
            "sha256": sha256(bytes),
            "at": jiff::Timestamp::now(),
        });
        let sources = self
            .home()
            .join("ai/projects/repo/agents/threads")
            .join(thread)
            .join("sources.jsonl");
        append(&sources, &format!("{record}\n"));
    }

    /// What `ai backup threads` prints, and whether it went well.
    fn threads(&self) -> (String, String, bool) {
        let o = self.backup(&["threads"]);
        (
            String::from_utf8(o.stdout).unwrap(),
            String::from_utf8(o.stderr).unwrap(),
            o.status.success(),
        )
    }

    /// What the last run left in `~/.ai/backup.json`.
    fn state(&self) -> Value {
        match fs::read(self.ai().join("backup.json")) {
            Ok(b) => serde_json::from_slice(&b).unwrap(),
            Err(_) => Value::Null,
        }
    }
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout.clone()).unwrap()
}

/// The error output of a command that failed.
fn failed(o: &Output) -> String {
    assert!(
        !o.status.success(),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8(o.stderr.clone()).unwrap()
}

fn append(p: &Path, text: &str) {
    use std::io::Write;
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut f = File::options().create(true).append(true).open(p).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

/// Waits until `done` holds, at most a minute.
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_first_run_makes_the_repository_and_its_key_and_later_runs_add_what_changed() {
    let m = Machine::new();
    let transcript = m.claude().join("p/s.jsonl");
    append(&transcript, "{\"n\":1}\n");

    let out = ok(&m.backup(&["run"]));
    assert!(out.contains("Made the repository ~/.ai/backup."), "{out}");
    assert!(out.contains("1 file new or changed"), "{out}");
    let key = m.ai().join("backup.key");
    assert_eq!(
        fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_to_string(&key).unwrap().trim().len(), 64);
    let snaps = m.snapshots(&m.repo(), &m.key());
    assert_eq!(snaps.len(), 1);
    assert_eq!(m.state()["snapshot"], snaps[0]["id"]);
    assert_eq!(
        m.dump(&m.repo(), &m.key(), &transcript).stdout,
        b"{\"n\":1}\n"
    );

    let out = ok(&m.backup(&["run"]));
    assert_eq!(out, "Nothing changed since the last snapshot.\n");
    assert_eq!(m.snapshots(&m.repo(), &m.key()).len(), 1);

    append(&transcript, "{\"n\":2}\n");
    append(&m.claude().join("p/t.jsonl"), "{}\n");
    let out = ok(&m.backup(&["run"]));
    assert!(out.contains("2 files new or changed"), "{out}");
    assert_eq!(m.snapshots(&m.repo(), &m.key()).len(), 2);
    assert_eq!(
        m.dump(&m.repo(), &m.key(), &transcript).stdout,
        b"{\"n\":1}\n{\"n\":2}\n"
    );

    let status = ok(&m.backup(&["status"]));
    assert!(status.contains("Repository: ~/.ai/backup\n"), "{status}");
    assert!(status.contains("  ~/.claude/projects\n"), "{status}");
    assert!(
        status.contains("  ~/.codex/sessions (missing)\n"),
        "{status}"
    );
    assert!(status.contains("Last run: "), "{status}");
    ok(&m.backup(&["check", "--read-data"]));
}

#[test]
fn a_linked_harness_folder_and_a_named_folder_are_backed_up_where_they_really_sit() {
    let m = Machine::new();
    let real = m.root.join("config/claude/projects");
    fs::create_dir_all(&real).unwrap();
    fs::remove_dir(m.claude()).unwrap();
    symlink(&real, m.claude()).unwrap();
    append(&real.join("s.jsonl"), "linked\n");
    append(&m.home().join("work/projects/w.jsonl"), "named\n");
    m.settings("[backup]\nfolders = [\"~/work/projects\"]\n");

    ok(&m.backup(&["run"]));
    let dump = |p: &Path| m.dump(&m.repo(), &m.key(), p);
    assert_eq!(dump(&real.join("s.jsonl")).stdout, b"linked\n");
    assert_eq!(
        dump(&m.home().join("work/projects/w.jsonl")).stdout,
        b"named\n"
    );
    assert!(!dump(&m.claude().join("s.jsonl")).status.success());
    let status = ok(&m.backup(&["status"]));
    assert!(
        status.contains(&format!("  ~/.claude/projects (at {})\n", real.display())),
        "{status}"
    );
}

#[test]
fn a_remote_gets_a_copy_of_every_snapshot_cut_into_the_same_chunks() {
    let m = Machine::new();
    let remote = m.root.join("remote");
    let transcript = m.claude().join("s.jsonl");
    append(&transcript, "one\n");
    ok(&m.backup(&["run"]));

    // The remote can't be opened yet: the snapshot is made, and the run says what failed.
    let settings = |password: &str| {
        m.settings(&format!(
            "[backup.remote]\nrepository = \"{}\"\npassword-command = \"{password}\"\n\
             env-command = \"echo AI_TEST_VARIABLE=1\"\n",
            remote.display()
        ));
    };
    settings("false");
    append(&transcript, "two\n");
    let e = failed(&m.backup(&["run"]));
    assert_eq!(
        e,
        "`[backup.remote] password-command = \"false\"` failed: exit status: 1.\n"
    );
    assert_eq!(m.snapshots(&m.repo(), &m.key()).len(), 2);
    assert_eq!(m.state()["copied"], Value::Null);
    let status = m.backup(&["status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("not yet copied to the remote"),
        "{status:?}"
    );
    assert!(failed(&status).contains("password-command"));

    settings("echo remote-password");
    let out = ok(&m.backup(&["run"]));
    assert!(
        out.contains("Nothing changed") && out.contains("Made the remote repository"),
        "{out}"
    );
    assert!(out.contains("Copied every snapshot to"), "{out}");
    let password = "--password-command=echo remote-password";
    let local = m.snapshots(&m.repo(), &m.key());
    let copies = m.snapshots(&remote, password);
    assert_eq!(copies.len(), 2);
    for (l, c) in local.iter().zip(&copies) {
        assert_eq!(c["original"], l["id"]);
        assert_eq!(c["tree"], l["tree"]);
    }
    assert_eq!(m.dump(&remote, password, &transcript).stdout, b"one\ntwo\n");
    let polynomial = |repo: &Path, password: &str| {
        let config = ok(&m.restic(repo, password, &["cat", "config"]));
        serde_json::from_str::<Value>(&config).unwrap()["chunker_polynomial"].clone()
    };
    assert_eq!(
        polynomial(&remote, password),
        polynomial(&m.repo(), &m.key())
    );

    // Once copied, a run with nothing new leaves the remote alone; a new snapshot is copied.
    assert_eq!(
        ok(&m.backup(&["run"])),
        "Nothing changed since the last snapshot.\n"
    );
    append(&transcript, "three\n");
    assert!(ok(&m.backup(&["run"])).contains("Copied every snapshot to"));
    assert_eq!(m.snapshots(&remote, password).len(), 3);
    assert!(ok(&m.backup(&["status"])).contains("copied to the remote."));
    ok(&m.backup(&["check"]));
}

#[test]
fn a_database_written_during_a_run_is_backed_up_as_a_whole_copy() {
    let m = Machine::new();
    let db = m.home().join(".local/share/opencode/opencode.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let live = Connection::open(&db).unwrap();
    live.pragma_update(None, "journal_mode", "wal").unwrap();
    live.execute_batch("create table t (x); insert into t values (0);")
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = thread::spawn({
        let stop = stop.clone();
        move || {
            let mut n = 1;
            while !stop.load(Ordering::Relaxed) {
                live.execute("insert into t values (?1)", [n]).unwrap();
                n += 1;
            }
            n
        }
    });
    ok(&m.backup(&["run"]));
    stop.store(true, Ordering::Relaxed);
    let written = writer.join().unwrap();

    let staged = m
        .ai()
        .join("backup-staging")
        .join(db.strip_prefix("/").unwrap());
    let copy = m.root.join("copy.db");
    fs::write(&copy, ok_bytes(&m.dump(&m.repo(), &m.key(), &staged))).unwrap();
    let copy = Connection::open(&copy).unwrap();
    let check: String = copy
        .query_row("pragma integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    let rows: i64 = copy
        .query_row("select count(*) from t", [], |r| r.get(0))
        .unwrap();
    assert!((1..=written).contains(&rows), "{rows} of {written}");
    assert!(
        !m.dump(&m.repo(), &m.key(), &db).status.success(),
        "the database itself is never read as a file"
    );
    let status = ok(&m.backup(&["status"]));
    assert!(
        status.contains("  ~/.local/share/opencode/opencode.db (as its copy ~/.ai/backup-staging/"),
        "{status}"
    );

    // Once a run has the database as its writer left it, its copy stays as it is, so there is
    // nothing new to back up.
    ok(&m.backup(&["run"]));
    assert_eq!(
        ok(&m.backup(&["run"])),
        "Nothing changed since the last snapshot.\n"
    );
}

fn ok_bytes(o: &Output) -> Vec<u8> {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o.stdout.clone()
}

#[test]
fn without_restic_a_run_fails_saying_so_and_status_tells_of_it() {
    let m = Machine::new();
    append(&m.claude().join("s.jsonl"), "x\n");
    let empty = m.root.join("bin");
    fs::create_dir_all(&empty).unwrap();
    let run = |args: &[&str]| {
        m.command(&["backup"])
            .args(args)
            .env("PATH", &empty)
            .output()
            .unwrap()
    };
    let e = failed(&run(&["run"]));
    assert!(e.contains("no restic program on PATH"), "{e}");
    assert!(!m.repo().exists());
    let status = run(&["status"]);
    let out = String::from_utf8_lossy(&status.stdout);
    assert!(
        out.contains("no run has gone without a problem yet"),
        "{out}"
    );
    assert!(failed(&status).contains("no restic program on PATH"));

    // With restic back, the next run makes the repository with the key the first one left.
    ok(&m.backup(&["run"]));
    assert_eq!(m.snapshots(&m.repo(), &m.key()).len(), 1);
    ok(&m.backup(&["status"]));
}

#[test]
fn a_second_run_waits_for_the_first() {
    let m = Machine::new();
    append(&m.claude().join("s.jsonl"), "x\n");
    let lock = File::create(m.ai().join("backup.lock")).unwrap();
    lock.lock().unwrap();
    let mut run = m
        .command(&["backup", "run"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(run.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    assert_eq!(line, "Waiting for another backup run to end.\n");
    assert!(ok(&m.backup(&["status"])).contains("A run is in progress.\n"));
    thread::sleep(Duration::from_millis(300));
    assert!(run.try_wait().unwrap().is_none());
    assert!(!m.repo().exists());

    lock.unlock().unwrap();
    assert!(run.wait().unwrap().success());
    assert_eq!(m.snapshots(&m.repo(), &m.key()).len(), 1);
}

#[test]
fn broken_settings_fail_ai_backup_alone_and_a_projects_file_may_not_hold_the_table() {
    let m = Machine::new();
    m.settings("[backup]\nevery = \"soon\"\n");
    for command in ["run", "status", "check", "watch"] {
        let e = failed(&m.backup(&[command]));
        assert!(e.contains("~/.ai/ai.toml"), "{command}: {e}");
        assert!(
            e.contains("`soon` is no time between runs"),
            "{command}: {e}"
        );
    }
    assert!(!m.repo().exists());
    ok(&m.command(&["memory", "init"]).output().unwrap());
    ok(&m.command(&["memory", "config"]).output().unwrap());

    m.settings("[backup]\nrepository = \"backups\"\n");
    let e = failed(&m.backup(&["run"]));
    assert!(
        e.contains("~/.ai/ai.toml: [backup] repository: 'backups' is neither an absolute path"),
        "{e}"
    );

    m.settings("");
    let project = m.home().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("ai.toml"),
        "[backup]\nrepository = \"/tmp/elsewhere\"\n",
    )
    .unwrap();
    let o = m
        .command(&["memory", "config"])
        .current_dir(&project)
        .output()
        .unwrap();
    let e = failed(&o);
    assert!(
        e.contains("ai.toml: [backup] belongs in your own ai.toml, not in a project's"),
        "{e}"
    );
    let o = m
        .command(&["backup", "status"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("Repository: ~/.ai/backup"),
        "ai backup reads no project's settings"
    );
}

#[test]
fn a_file_that_cant_be_read_fails_the_run_which_still_backs_up_the_rest() {
    let m = Machine::new();
    let (good, bad) = (m.claude().join("good.jsonl"), m.claude().join("bad.jsonl"));
    append(&good, "good\n");
    append(&bad, "bad\n");
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&bad).is_ok() {
        return; // Run as root, which reads everything.
    }
    let e = failed(&m.backup(&["run"]));
    assert!(
        e.contains("bad.jsonl") && e.contains("permission denied"),
        "{e}"
    );
    assert_eq!(m.dump(&m.repo(), &m.key(), &good).stdout, b"good\n");
    assert!(m.state()["error"].as_str().unwrap().contains("bad.jsonl"));
    assert_eq!(m.state()["ok"], Value::Null);
    assert!(failed(&m.backup(&["status"])).contains("bad.jsonl"));

    fs::set_permissions(&bad, fs::Permissions::from_mode(0o644)).unwrap();
    ok(&m.backup(&["run"]));
    assert_eq!(m.dump(&m.repo(), &m.key(), &bad).stdout, b"bad\n");
    assert_eq!(m.state()["error"], Value::Null);
}

#[test]
fn a_folder_that_is_missing_or_holds_the_repository_and_a_lost_key_are_refused() {
    let m = Machine::new();
    append(&m.claude().join("s.jsonl"), "x\n");
    m.settings("[backup]\nfolders = [\"~/nowhere\"]\n");
    let e = failed(&m.backup(&["run"]));
    assert!(
        e.contains("[backup] folders names ~/nowhere, which does not exist."),
        "{e}"
    );
    assert_eq!(
        m.snapshots(&m.repo(), &m.key()).len(),
        1,
        "the rest went in"
    );

    m.settings("[backup]\nfolders = [\"~/.ai\"]\n");
    let e = failed(&m.backup(&["run"]));
    assert!(
        e.contains("~/.ai holds or sits in the repository ~/.ai/backup"),
        "{e}"
    );

    m.settings("");
    fs::remove_file(m.ai().join("backup.key")).unwrap();
    let e = failed(&m.backup(&["run"]));
    assert!(
        e.contains("~/.ai/backup.key is gone, and it holds the password of the repository"),
        "{e}"
    );
    assert!(!m.ai().join("backup.key").exists(), "no new key is made");
}

#[test]
fn a_sessions_end_starts_a_run_once_the_repository_is_made_unless_switched_off() {
    let m = Machine::new();
    let transcript = m.claude().join("s.jsonl");
    append(&transcript, "one\n");
    let end = || {
        let o = m.command(&["agent", "end-session"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    };

    // No repository yet: backups begin with a run, not with a session.
    end();
    thread::sleep(Duration::from_millis(500));
    assert!(!m.repo().exists() && m.state().is_null());

    ok(&m.backup(&["run"]));
    let first = m.state()["snapshot"].clone();
    m.settings("[backup]\non-session-end = false\n");
    append(&transcript, "two\n");
    end();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(m.state()["snapshot"], first);

    m.settings("");
    end();
    until("the run a session's end started", || {
        m.state()["snapshot"] != first
    });
    until("that run to end", || {
        File::open(m.ai().join("backup.lock"))
            .unwrap()
            .try_lock()
            .is_ok()
    });
    assert_eq!(
        m.dump(&m.repo(), &m.key(), &transcript).stdout,
        b"one\ntwo\n"
    );
}

#[test]
fn watch_runs_again_and_again_and_goes_on_after_a_run_that_failed() {
    let m = Machine::new();
    let transcript = m.claude().join("s.jsonl");
    append(&transcript, "one\n");
    m.settings("[backup]\nevery = \"1s\"\nfolders = [\"~/later\"]\n");
    let mut watch = m
        .command(&["backup", "watch"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let snapshots = || m.state()["snapshot"].as_str().map(str::to_owned);
    until("the first snapshot", || snapshots().is_some());
    let first = snapshots();
    let mut err = BufReader::new(watch.stderr.take().unwrap());
    let mut line = String::new();
    err.read_line(&mut line).unwrap();
    assert_eq!(
        line,
        "[backup] folders names ~/later, which does not exist.\n"
    );

    append(&m.home().join("later/l.jsonl"), "later\n");
    append(&transcript, "two\n");
    until("the next snapshot", || snapshots() != first);
    until("a run without a problem", || !m.state()["ok"].is_null());
    until("the watch to rest between runs", || {
        File::open(m.ai().join("backup.lock"))
            .unwrap()
            .try_lock()
            .is_ok()
    });
    watch.kill().unwrap();
    watch.wait().unwrap();
    assert_eq!(
        m.dump(&m.repo(), &m.key(), &transcript).stdout,
        b"one\ntwo\n"
    );
}

#[test]
fn status_tells_whether_a_watch_is_running_and_a_second_watch_is_turned_away() {
    let m = Machine::new();
    append(&m.claude().join("s.jsonl"), "x\n");
    m.settings("[backup]\nevery = \"1h\"\n");
    let off = "Runs: at each session's end and on `ai backup run`; `ai backup watch` (every 1h) \
               isn't running: start it, or enable services.ai-tools.backup in Home Manager\n";
    assert!(ok(&m.backup(&["status"])).contains(off));

    let mut watch = m
        .command(&["backup", "watch"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lock = m.ai().join("backup-watch.lock");
    until("the watch to hold its lock", || {
        File::open(&lock).is_ok_and(|f| f.try_lock().is_err())
    });
    until("the watch's first run", || !m.state()["ok"].is_null());
    until("the watch to rest between runs", || {
        File::open(m.ai().join("backup.lock"))
            .unwrap()
            .try_lock()
            .is_ok()
    });
    let status = ok(&m.backup(&["status"]));
    assert!(
        status.contains(
            "Runs: every 1h under `ai backup watch` (running), and at each session's end\n"
        ),
        "{status}"
    );
    let e = failed(&m.backup(&["watch"]));
    assert_eq!(
        e,
        "Another `ai backup watch` is running: it holds ~/.ai/backup-watch.lock.\n"
    );
    assert!(watch.try_wait().unwrap().is_none());

    watch.kill().unwrap();
    watch.wait().unwrap();
    assert!(ok(&m.backup(&["status"])).contains(off));
}

#[test]
fn status_on_a_home_that_never_ran_a_backup_says_so_and_writes_nothing() {
    let m = Machine::new();
    fs::remove_dir_all(m.ai()).unwrap();
    let status = ok(&m.backup(&["status"]));
    assert!(
        status.contains("(not made yet: `ai backup run` makes it)"),
        "{status}"
    );
    assert!(status.ends_with("No run yet.\n"), "{status}");
    assert!(!m.ai().exists());
    let e = failed(&m.backup(&["check"]));
    assert!(e.contains("No repository at ~/.ai/backup to check"), "{e}");
}

#[test]
fn threads_tells_where_what_each_thread_read_is_and_fails_on_what_is_lost() {
    let m = Machine::new();
    let host = host().unwrap();
    let a = m.claude().join("p/a.jsonl");
    append(&a, "one\n");
    m.read_by("dev", &host, &a, 0, b"one\n");

    let (out, err, ok_) = m.threads();
    assert_eq!(
        out,
        "Threads in ~/ai read 1 file: 0 backed up, 1 with no backup.\n\
         No repository yet: `ai backup run` makes it.\n"
    );
    assert!(ok_ && err.is_empty(), "{err}");
    assert!(!m.repo().exists(), "a check makes no repository");

    ok(&m.backup(&["run"]));
    let (out, err, ok_) = m.threads();
    assert_eq!(out, "Threads in ~/ai read 1 file: 1 backed up.\n");
    assert!(ok_ && err.is_empty(), "{err}");

    // The file grew, and the thread read on: not in a snapshot until the next run.
    append(&a, "two\n");
    m.read_by("dev", &host, &a, 4, b"one\ntwo\n");
    let (out, _, ok_) = m.threads();
    assert_eq!(
        out,
        "Threads in ~/ai read 1 file: 0 backed up, 1 not yet backed up.\n"
    );
    assert!(ok_);
    ok(&m.backup(&["run"]));

    // Gone from the harness's folder, its bytes are read back from the repository. A file
    // changed under the thread has a version in an older snapshot and one in the file.
    fs::remove_file(&a).unwrap();
    let odd = m.claude().join("p/c[1]*.jsonl");
    append(&odd, "first version\n");
    m.read_by("dev", &host, &odd, 0, b"first version\n");
    ok(&m.backup(&["run"]));
    fs::write(&odd, "second\n").unwrap();
    m.read_by("dev", &host, &odd, 0, b"second\n");
    ok(&m.backup(&["run"]));
    append(
        &m.claude().join("p/c11.jsonl"),
        "another file the pattern must not match\n",
    );
    ok(&m.backup(&["run"]));
    let (out, err, ok_) = m.threads();
    assert_eq!(out, "Threads in ~/ai read 2 files: 2 backed up.\n");
    assert!(ok_ && err.is_empty(), "{err}");

    // Bytes neither a file nor a snapshot holds are lost, and so are bytes a snapshot holds
    // only part of. A thread whose records can't be read fails the check too.
    let never = m.claude().join("p/b.jsonl");
    m.read_by("other", &host, &never, 0, b"never\n");
    m.read_by("other", &host, &a, 0, b"one\ntwo\nthree\n");
    m.read_by("away", "elsewhere", Path::new("/far/s.jsonl"), 0, b"far\n");
    append(
        &m.home()
            .join("ai/projects/repo/agents/threads/bad/sources.jsonl"),
        "{\"hash\":\"abc\"}\n",
    );
    let (out, err, ok_) = m.threads();
    assert_eq!(
        out,
        "Threads in ~/ai read 5 files: 2 backed up, 1 on another machine, 2 lost.\n\
         \x20 /far/s.jsonl: repo.away read 4B of it on elsewhere, and no repository here holds \
         them.\n"
    );
    assert_eq!(
        err,
        "Thread repo.bad: source record 1 is corrupt: missing field `harness` at line 1 column \
         14\n\
         ~/.claude/projects/p/a.jsonl: the 14B repo.other read of it are lost: the file no \
         longer holds them, and no repository does.\n\
         ~/.claude/projects/p/b.jsonl: the 6B repo.other read of it are lost: the file no \
         longer holds them, and no repository does.\n"
    );
    assert!(!ok_);
}

#[test]
fn what_a_thread_read_comes_back_from_the_remote_and_a_database_from_its_copy() {
    let m = Machine::new();
    let host = host().unwrap();
    let remote = m.root.join("remote");
    m.settings(&format!(
        "[backup.remote]\nrepository = \"{}\"\npassword-command = \"echo remote-password\"\n",
        remote.display()
    ));
    let transcript = m.claude().join("s.jsonl");
    append(&transcript, "one\n");
    let db = m.home().join(".local/share/opencode/opencode.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES ('kept');")
        .unwrap();
    ok(&m.backup(&["run"]));
    let staged = m
        .ai()
        .join("backup-staging")
        .join(db.strip_prefix("/").unwrap());
    let copy = fs::read(&staged).unwrap();
    m.read_by("dev", &host, &transcript, 0, b"one\n");
    m.read_by("dev", &host, &db, 0, &copy);

    // The database itself may differ from its copy by then; the copy in the snapshot serves.
    fs::remove_file(&db).unwrap();
    fs::remove_file(&transcript).unwrap();
    let (out, err, ok_) = m.threads();
    assert_eq!(out, "Threads in ~/ai read 2 files: 2 backed up.\n");
    assert!(ok_ && err.is_empty(), "{err}");

    // With the local repository gone, the remote one holds them still.
    fs::remove_dir_all(m.repo()).unwrap();
    fs::remove_dir_all(m.ai().join("backup-staging")).unwrap();
    let (out, err, ok_) = m.threads();
    assert_eq!(out, "Threads in ~/ai read 2 files: 2 backed up.\n");
    assert!(ok_ && err.is_empty(), "{err}");

    // A password command that fails is said, and the check fails rather than call bytes lost.
    m.settings(&format!(
        "[backup.remote]\nrepository = \"{}\"\npassword-command = \"exit 3\"\n",
        remote.display()
    ));
    let (out, err, ok_) = m.threads();
    assert_eq!(out, "");
    assert_eq!(
        err,
        "`[backup.remote] password-command = \"exit 3\"` failed: exit status: 3.\n"
    );
    assert!(!ok_);
}

#[test]
fn threads_needs_no_restic_while_every_file_holds_what_was_read() {
    let m = Machine::new();
    let transcript = m.claude().join("s.jsonl");
    append(&transcript, "one\n");
    m.read_by("dev", &host().unwrap(), &transcript, 0, b"one\n");
    let o = m
        .command(&["backup", "threads"])
        .env("PATH", m.root.join("empty"))
        .output()
        .unwrap();
    assert!(ok(&o).contains("1 with no backup"));
}
