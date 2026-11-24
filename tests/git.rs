//! `ai` committing in a repository while git commits and packs its refs there by hand: neither
//! loses a commit, and no commit takes back what another made.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use tempfile::TempDir;

struct Sandbox {
    tmp: TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let s = Sandbox {
            tmp: TempDir::new().unwrap(),
        };
        fs::create_dir(s.tmp.path().join("home")).unwrap();
        s
    }

    fn notes(&self) -> PathBuf {
        self.tmp.path().join("notes")
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
            .env("AI_NOTES_DIR", self.notes())
            .env("AI_NOTES_SUMMARISE", "0")
            .current_dir(self.tmp.path())
            .args(["notes", "add", "a.b", "A claim."])
            .args(["--confidence", "reported", "--title", title])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        c
    }

    /// `git args` run by hand in the notes' repository.
    fn git(&self, args: &[&str]) -> Output {
        git_in(&self.notes(), args)
    }

    /// `git args`, which must succeed; its output.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.git(args);
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
        String::from_utf8(out.stdout).unwrap()
    }
}

fn git_in(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Hand")
        .env("GIT_AUTHOR_EMAIL", "hand@localhost")
        .env("GIT_COMMITTER_NAME", "Hand")
        .env("GIT_COMMITTER_EMAIL", "hand@localhost")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Whether a git commit failed only as git fails beside another writer, having changed nothing:
/// for a lock another held (git takes the index's without waiting), or for HEAD moved after git
/// read it.
fn locked_out(o: &Output) -> bool {
    let e = String::from_utf8_lossy(&o.stderr);
    e.contains(".lock': File exists") || e.contains("but expected")
}

/// Tells the helpers to stop when dropped, as on a failed assertion, so the scope ends.
struct Stop<'a>(&'a AtomicBool);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[test]
fn commits_by_ai_and_by_hand_while_git_packs_the_refs_lose_nothing() {
    let s = Sandbox::new();
    let first = s.add("first").output().unwrap();
    assert!(first.status.success(), "{}", text(&first));
    // Waits as long as a loaded machine needs, so that no commit fails for a held ref lock.
    s.ok(&["config", "core.filesRefLockTimeout", "60000"]);
    s.ok(&["config", "core.packedRefsTimeout", "60000"]);
    s.ok(&["pack-refs", "--all", "--prune"]);

    let (rounds, n) = (4, 16);
    let done = AtomicBool::new(false);
    let (by_hand, packings) = thread::scope(|scope| {
        let hand = scope.spawn(|| {
            let mut made = Vec::new();
            while !done.load(Ordering::Relaxed) {
                let msg = format!("by hand {}", made.len());
                let out = s.git(&["commit", "--allow-empty", "-q", "-m", &msg]);
                match out.status.success() {
                    true => made.push(msg),
                    false => assert!(locked_out(&out), "{}", text(&out)),
                }
            }
            made
        });
        let packer = scope.spawn(|| {
            let mut packings = 0;
            while !done.load(Ordering::Relaxed) {
                s.ok(&["pack-refs", "--all", "--prune"]);
                packings += 1;
            }
            packings
        });
        let stop = Stop(&done);
        for round in 0..rounds {
            let kids: Vec<_> = (0..n)
                .map(|i| s.add(&format!("parallel {round} {i}")).spawn().unwrap())
                .collect();
            for k in kids {
                let r = k.wait_with_output().unwrap();
                assert!(r.status.success(), "{}", text(&r));
            }
        }
        drop(stop);
        (hand.join().unwrap(), packer.join().unwrap())
    });
    assert!(!by_hand.is_empty(), "no commit by hand ran beside ai's");
    assert!(packings > 0);

    let subjects = s.ok(&["log", "--format=%s"]);
    let subjects: Vec<&str> = subjects.lines().collect();
    assert_eq!(
        subjects.iter().filter(|l| **l == "add a.b").count(),
        rounds * n + 1,
        "{subjects:?}"
    );
    for msg in &by_hand {
        assert_eq!(subjects.iter().filter(|l| *l == msg).count(), 1, "{msg}");
    }
    assert_eq!(subjects.len(), rounds * n + 1 + by_hand.len());
    assert_eq!(
        s.ok(&["log", "--diff-filter=DM", "--name-only", "--format="]),
        "",
        "a commit took back a note"
    );
    assert_eq!(s.ok(&["ls-files", "a/b"]).lines().count(), rounds * n + 1);
    assert_eq!(s.ok(&["status", "--porcelain"]), "");
    assert_eq!(
        s.ok(&["reflog", "show", "--format=%gs", "HEAD"])
            .lines()
            .count(),
        subjects.len()
    );
    s.ok(&["fsck", "--strict"]);
}
