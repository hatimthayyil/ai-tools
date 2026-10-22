use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use ai_tools::project::Project;
use tempfile::TempDir;

const ISOLATE: [&str; 6] = [
    "AI_FILES_DIR",
    "AI_MEMORY_MODEL",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_AUTHOR_NAME",
    "GIT_COMMITTER_NAME",
];

/// A committed git repository of a few files, made a project, and a fake `claude`, in a
/// temporary directory.
/// The fake answers each request with `summary of <path>`, logging the path to `out/asked`,
/// and fails a path holding `bad`.
struct Sandbox {
    _tmp: TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for d in ["home", "out", "bin", "repo/src", "repo/target"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let s = Sandbox { _tmp: tmp, root };
        s.write("README.md", "# A project\n");
        s.write("src/a.rs", "fn a() {}\n");
        s.write("src/b.rs", "fn b() {}\n");
        s.write(".gitignore", "target/\n");
        s.write("target/x.rs", "fn x() {}\n");
        s.write("logo.png", "\0\x01binary");
        Project::init(&s.repo()).unwrap();
        gix::init(s.repo()).unwrap();
        let commit = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(["commit", "-q", "--allow-empty", "--no-verify", "-m", "init"])
            .current_dir(s.repo())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(commit.success());
        s.fake_claude();
        s
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn write(&self, path: &str, text: &str) {
        fs::write(self.repo().join(path), text).unwrap();
    }

    fn fake_claude(&self) {
        let asked = self.root.join("out/asked");
        let body = format!(
            r#"#!/bin/sh
while [ "$1" != --session-id ]; do shift; done
while read -r line; do
  p=$(printf '%s' "$line" | sed -nE 's/.*Summarise the (file|directory) ([^ ]+) .*/\2/p')
  printf '%s\n' "$p" >> {asked}
  case $p in
    *bad*) printf '{{"type":"result","is_error":true,"result":"overloaded","session_id":"%s"}}\n' "$2" ;;
    *) printf '{{"type":"result","is_error":false,"result":"summary of %s","total_cost_usd":0.01,"session_id":"%s"}}\n' "$p" "$2" ;;
  esac
done
"#,
            asked = asked.display()
        );
        let dst = self.root.join("bin/claude");
        let mut sh = Command::new("sh")
            .args(["-c", r#"cat >"$1" && chmod 755 "$1""#, "sh"])
            .arg(&dst)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        sh.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
        assert!(sh.wait().unwrap().success());
    }

    /// `ai files args` run in `dir`.
    fn ai_in(&self, dir: &Path, args: &[&str]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ai"));
        for k in ISOLATE {
            c.env_remove(k);
        }
        let path = std::env::var("PATH").unwrap();
        c.current_dir(dir)
            .arg("files")
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env(
                "PATH",
                format!("{}:{path}", self.root.join("bin").display()),
            )
            .output()
            .unwrap()
    }

    fn ai(&self, args: &[&str]) -> Output {
        self.ai_in(&self.repo(), args)
    }

    /// The paths the fake `claude` was asked to summarise since the last call, sorted.
    fn asked(&self) -> Vec<String> {
        let p = self.root.join("out/asked");
        let mut asked: Vec<String> = fs::read_to_string(&p)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let _ = fs::remove_file(p);
        asked.sort();
        asked
    }

    /// The project's store: the one directory under `~/.ai/files`.
    fn store(&self) -> PathBuf {
        let mut dirs: Vec<PathBuf> = fs::read_dir(self.root.join("home/.ai/files"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        dirs.pop().unwrap()
    }
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn fails(o: &Output) -> String {
    assert!(
        !o.status.success(),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8(o.stderr.clone()).unwrap()
}

#[test]
fn a_build_summarises_files_then_directories_and_view_prints_them() {
    let s = Sandbox::new();
    assert_eq!(
        ok(&s.ai(&["build"])),
        "Wrote 7 summaries in 7 requests to the model, $0.07.\n"
    );
    let order = fs::read_to_string(s.root.join("out/asked")).unwrap();
    let at = |p: &str| order.lines().position(|l| l == p).unwrap();
    assert!(at("src/a.rs") < at("src") && at("src/b.rs") < at("src"));
    assert!(at("README.md") < at(".") && at("src") < at("."));
    assert_eq!(
        s.asked(),
        [
            ".",
            ".gitignore",
            "README.md",
            "ai.toml",
            "src",
            "src/a.rs",
            "src/b.rs"
        ]
    );
    let store = s.store();
    assert_eq!(
        fs::read_to_string(store.join("summaries.jsonl"))
            .unwrap()
            .lines()
            .count(),
        7
    );
    assert_eq!(fs::read_dir(store.join("calls")).unwrap().count(), 7);
    assert_eq!(ok(&s.ai(&["view"])), "summary of .\n");
    assert_eq!(ok(&s.ai(&["view", "src/a.rs"])), "summary of src/a.rs\n");
    let src = s.repo().join("src");
    assert_eq!(
        ok(&s.ai_in(&src, &["view", "b.rs"])),
        "summary of src/b.rs\n"
    );
    assert_eq!(ok(&s.ai_in(&src, &["view", "."])), "summary of src\n");
}

#[test]
fn a_build_summarises_only_what_changed_since_the_last() {
    let s = Sandbox::new();
    ok(&s.ai(&["build"]));
    s.asked();
    assert_eq!(ok(&s.ai(&["build"])), "Nothing to build.\n");
    assert!(s.asked().is_empty());
    s.write("src/a.rs", "fn a() { b() }\n");
    assert_eq!(
        ok(&s.ai(&["build"])),
        "Wrote 3 summaries in 3 requests to the model, $0.03.\n"
    );
    assert_eq!(s.asked(), [".", "src", "src/a.rs"]);
}

#[test]
fn view_fails_on_what_has_no_summary_naming_why() {
    let s = Sandbox::new();
    let e = fails(&s.ai(&["view", "src/a.rs"]));
    assert_eq!(
        e,
        "src/a.rs has no summary as it is now. Run ai files build.\n"
    );
    ok(&s.ai(&["build"]));
    for p in ["target/x.rs", "logo.png"] {
        let e = fails(&s.ai(&["view", p]));
        assert_eq!(
            e,
            format!("ai files leaves {p} out: git ignores it, or it holds no text.\n")
        );
    }
    s.write("src/b.rs", "fn b() { a() }\n");
    let e = fails(&s.ai(&["view", "src"]));
    assert_eq!(e, "src has no summary as it is now. Run ai files build.\n");
    assert_eq!(ok(&s.ai(&["view", "src/a.rs"])), "summary of src/a.rs\n");
    let e = fails(&s.ai(&["view", "../out"]));
    assert!(e.starts_with("../out is outside the project at "), "{e}");
}

#[test]
fn a_failed_call_stops_the_build_and_keeps_what_was_written() {
    let s = Sandbox::new();
    s.write("src/bad.rs", "fn bad() {}\n");
    let e = fails(&s.ai(&["build"]));
    assert!(
        e.starts_with("Stopped at src/bad.rs: "),
        "the failed path and its cause: {e}"
    );
    assert!(
        e.ends_with(
            "The 5 summaries written so far are kept; the next ai files build goes on from them.\n"
        ),
        "{e}"
    );
    assert_eq!(
        s.asked(),
        [
            ".gitignore",
            "README.md",
            "ai.toml",
            "src/a.rs",
            "src/b.rs",
            "src/bad.rs"
        ]
    );
    fs::remove_file(s.repo().join("src/bad.rs")).unwrap();
    assert_eq!(
        ok(&s.ai(&["build"])),
        "Wrote 2 summaries in 2 requests to the model, $0.02.\n"
    );
    assert_eq!(s.asked(), [".", "src"]);
}

#[test]
fn outside_a_project_it_says_how_to_make_one() {
    let s = Sandbox::new();
    let e = fails(&s.ai_in(&s.root.join("out"), &["build"]));
    assert_eq!(
        e,
        "Not in a project: no ai.toml here or above. Run `ai init` in the project's top \
         directory.\n"
    );
}
