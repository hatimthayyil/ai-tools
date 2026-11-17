//! An `ai` that a signal ends mid-commit leaves none of git's lock files behind.

use std::{
    fs,
    io::Write,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

/// A notes store whose next commit stops while it holds `HEAD.lock`: the lock of the branch HEAD
/// names is held, and the repository's config has ref locks waited on for a minute.
struct Stalled {
    tmp: TempDir,
}

impl Stalled {
    fn new() -> Stalled {
        let s = Stalled {
            tmp: TempDir::new().unwrap(),
        };
        fs::create_dir(s.tmp.path().join("home")).unwrap();
        let out = s.add("first").output().unwrap();
        assert!(out.status.success(), "{}", text(&out));
        let mut config = fs::OpenOptions::new()
            .append(true)
            .open(s.git_dir().join("config"))
            .unwrap();
        writeln!(config, "[core]\n\tfilesRefLockTimeout = 60000").unwrap();
        fs::write(s.branch_lock(), "").unwrap();
        s
    }

    fn git_dir(&self) -> PathBuf {
        self.tmp.path().join("notes/.git")
    }

    fn branch_lock(&self) -> PathBuf {
        let head = fs::read_to_string(self.git_dir().join("HEAD")).unwrap();
        let branch = head.trim().strip_prefix("ref: ").unwrap();
        self.git_dir().join(format!("{branch}.lock"))
    }

    fn head_lock(&self) -> PathBuf {
        self.git_dir().join("HEAD.lock")
    }

    /// `ai notes add` of a note `title`.
    fn add(&self, title: &str) -> Command {
        let home = self.tmp.path().join("home");
        let mut c = Command::new(env!("CARGO_BIN_EXE_ai"));
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("AI_NOTES_DIR", self.tmp.path().join("notes"))
            .env("AI_NOTES_SUMMARISE", "0")
            .current_dir(self.tmp.path())
            .args(["notes", "add", "a.b", "A claim."])
            .args(["--confidence", "reported", "--title", title])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    /// Starts `c` and returns once it holds `HEAD.lock`, waiting for the branch's.
    fn stall(&self, mut c: Command) -> Running {
        let r = Running(Some(c.spawn().unwrap()));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.head_lock().exists() {
            assert!(Instant::now() < deadline, "ai never took HEAD.lock");
            thread::sleep(Duration::from_millis(5));
        }
        r
    }
}

/// A child killed if the test ends before it does.
struct Running(Option<Child>);

impl Running {
    fn signal(&self, sig: libc::c_int) {
        // SAFETY: kill takes a pid and a signal; the pid is our own unreaped child's.
        assert_eq!(
            unsafe { libc::kill(self.0.as_ref().unwrap().id() as libc::pid_t, sig) },
            0
        );
    }

    fn wait(mut self) -> Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(c) = &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn a_signal_that_ends_a_commit_takes_its_locks_with_it() {
    for sig in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        let s = Stalled::new();
        let ai = s.stall(s.add("second"));
        ai.signal(sig);
        let out = ai.wait();
        assert_eq!(out.status.signal(), Some(sig), "{}", text(&out));
        assert!(!s.head_lock().exists(), "signal {sig} left HEAD.lock");
        assert!(!s.git_dir().join("index.lock").exists());
        assert!(
            s.branch_lock().exists(),
            "ai removed a lock it did not take"
        );
    }
}

#[test]
fn a_signal_ignored_when_ai_starts_stays_ignored() {
    let s = Stalled::new();
    let mut c = s.add("second");
    // SAFETY: between fork and exec this only sets a signal's disposition, which is
    // async-signal-safe.
    unsafe {
        c.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    let ai = s.stall(c);
    ai.signal(libc::SIGHUP);
    fs::remove_file(s.branch_lock()).unwrap();
    let out = ai.wait();
    assert!(out.status.success(), "{}", text(&out));
    assert!(!s.head_lock().exists());
}
