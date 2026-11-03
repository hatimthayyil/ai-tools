use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

use ai_tools::harness::{self, Env, Out, Record, State, backup};
use jiff::{SignedDuration, Timestamp};
use tempfile::TempDir;

/// A made-up machine: a home, its `~/.ai`, and a process list, in a temporary directory.
struct Machine {
    _tmp: TempDir,
    root: PathBuf,
    vars: HashMap<String, String>,
    now: Timestamp,
    room: u64,
}

/// What one command printed, and whether it needs the user.
struct Ran {
    out: String,
    err: String,
    failed: bool,
}

impl Machine {
    fn new() -> Machine {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("proc/self")).unwrap();
        Machine {
            _tmp: tmp,
            root,
            vars: HashMap::new(),
            now: Timestamp::from_second(1_800_000_000).unwrap(),
            room: u64::MAX,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn harness(&self) -> PathBuf {
        self.home().join(".ai/harness")
    }

    fn env(&self) -> Env {
        let vars = self.vars.clone();
        let room = self.room;
        Env {
            home: self.home(),
            ai: self.home().join(".ai"),
            var: Box::new(move |k| vars.get(k).cloned()),
            proc: self.root.join("proc"),
            free: if room == u64::MAX {
                |_| Ok(u64::MAX)
            } else {
                |_| Ok(0)
            },
            now: self.now,
        }
    }

    fn run(&self, cmd: fn(&Env, &mut Out) -> anyhow::Result<()>) -> Ran {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let failed = {
            let mut out = Out::new(&mut o, &mut e);
            cmd(&self.env(), &mut out).unwrap();
            out.failed
        };
        Ran {
            out: String::from_utf8(o).unwrap(),
            err: String::from_utf8(e).unwrap(),
            failed,
        }
    }

    /// A process `pid` with `args`, holding open each of `files`.
    fn process(&self, pid: u32, args: &[&str], files: &[&Path]) {
        let d = self.root.join(format!("proc/{pid}"));
        fs::create_dir_all(d.join("fd")).unwrap();
        fs::write(d.join("cmdline"), args.join("\0") + "\0").unwrap();
        symlink("/", d.join("cwd")).unwrap();
        for (i, f) in files.iter().enumerate() {
            symlink(f, d.join(format!("fd/{i}"))).unwrap();
        }
    }

    /// Claude Code's transcripts, as it leaves them in `~/.claude/projects`.
    fn claude(&self) -> PathBuf {
        let p = self.home().join(".claude/projects");
        write(&p.join("-work/s1.jsonl"), "{\"a\":1}\n");
        write(&p.join("-work/s1/subagents/a1.jsonl"), "{\"b\":2}\n");
        let f = p.join("-work/s1.jsonl");
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        let old = fs::FileTimes::new().set_modified(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
        );
        fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_times(old)
            .unwrap();
        symlink("s1.jsonl", p.join("-work/latest")).unwrap();
        p
    }

    fn state(&self, name: &str) -> State {
        let env = self.env();
        let p = env
            .places()
            .into_iter()
            .find(|p| p.path.ends_with(name))
            .unwrap();
        p.state(fs::canonicalize(env.base()).ok().as_deref())
            .unwrap()
    }
}

fn write(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

#[test]
fn link_links_absent_folders_and_waits_for_files() {
    let m = Machine::new();
    let r = m.run(harness::link);
    assert!(!r.failed, "{}", r.err);
    let projects = m.home().join(".claude/projects");
    assert!(is_link(&projects));
    assert_eq!(
        fs::read_link(&projects).unwrap(),
        m.harness().join("claude/projects")
    );
    assert!(m.harness().join("claude/projects").is_dir());
    assert!(is_link(&m.home().join(".hermes/sessions")));
    // A database's link waits for the database: SQLite must not meet a link to nothing.
    assert!(!m.home().join(".local/share/opencode/opencode.db").exists());
    assert!(!is_link(
        &m.home().join(".local/share/opencode/opencode.db")
    ));
    assert_eq!(
        fs::metadata(m.harness()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let again = m.run(harness::link);
    assert!(again.out.is_empty(), "{}", again.out);
}

#[test]
fn link_never_moves_data() {
    let m = Machine::new();
    let p = m.claude();
    let r = m.run(harness::link);
    assert!(r.out.contains("the next boot moves it"), "{}", r.out);
    assert!(!is_link(&p));
    assert_eq!(read(&p.join("-work/s1.jsonl")), "{\"a\":1}\n");
}

#[test]
fn move_copies_checks_links_and_keeps_the_original() {
    let m = Machine::new();
    let p = m.claude();
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(r.out.contains("Moved"), "{}", r.out);
    assert!(is_link(&p));
    let target = m.harness().join("claude/projects");
    assert_eq!(read(&target.join("-work/s1.jsonl")), "{\"a\":1}\n");
    assert_eq!(read(&p.join("-work/s1/subagents/a1.jsonl")), "{\"b\":2}\n");
    assert_eq!(
        fs::read_link(target.join("-work/latest")).unwrap(),
        Path::new("s1.jsonl")
    );
    let copied = fs::metadata(target.join("-work/s1.jsonl")).unwrap();
    assert_eq!(copied.permissions().mode() & 0o777, 0o600);
    assert_eq!(copied.mtime(), 1_700_000_000);
    assert_eq!(
        read(&backup(&p).join("-work/s1.jsonl")),
        "{\"a\":1}\n",
        "the original stays as the backup"
    );
    let record = Record::load(&m.harness().join("claude/projects.move.json"))
        .unwrap()
        .unwrap();
    assert!(record.verified);
    assert_eq!(record.from, p);
    assert_eq!(m.state("projects"), State::Linked(p));
}

#[test]
fn a_folder_whose_harness_runs_stays_where_it_is() {
    let m = Machine::new();
    let p = m.claude();
    m.process(100, &["/nix/store/x-claude/bin/claude", "--resume"], &[]);
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(r.out.contains("Left"), "{}", r.out);
    assert!(
        r.out.contains("100 /nix/store/x-claude/bin/claude"),
        "{}",
        r.out
    );
    assert!(!is_link(&p));
    assert!(!m.harness().join("claude/projects").exists());
}

#[test]
fn a_folder_with_a_file_open_stays_where_it_is() {
    let m = Machine::new();
    let p = m.claude();
    m.process(200, &["vim"], &[&p.join("-work/s1.jsonl")]);
    let r = m.run(harness::run);
    assert!(r.out.contains("Left"), "{}", r.out);
    assert!(!is_link(&p));
}

#[test]
fn other_processes_do_not_hold_a_folder() {
    let m = Machine::new();
    let p = m.claude();
    m.process(300, &["bash", "-l"], &[Path::new("/dev/null")]);
    m.process(301, &["pi-agent-helper"], &[]);
    let r = m.run(harness::run);
    assert!(r.out.contains("Moved"), "{}", r.out);
    assert!(is_link(&p));
}

#[test]
fn a_database_waits_for_its_wal_and_then_moves() {
    let m = Machine::new();
    let db = m.home().join(".local/share/opencode/opencode.db");
    write(&db, "sqlite pages");
    write(
        &m.home().join(".local/share/opencode/opencode.db-wal"),
        "pages",
    );
    let r = m.run(harness::run);
    assert!(r.failed);
    assert!(r.err.contains("opencode.db-wal is not empty"), "{}", r.err);
    assert!(!is_link(&db));

    fs::write(m.home().join(".local/share/opencode/opencode.db-wal"), "").unwrap();
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(is_link(&db));
    assert_eq!(
        read(&m.harness().join("opencode/opencode.db")),
        "sqlite pages"
    );
}

#[test]
fn a_target_holding_other_data_is_refused() {
    let m = Machine::new();
    let p = m.claude();
    write(&m.harness().join("claude/projects/other.jsonl"), "x");
    let r = m.run(harness::run);
    assert!(r.failed);
    assert!(r.err.contains("already holds other data"), "{}", r.err);
    assert!(!is_link(&p));
    assert!(!backup(&p).exists());
}

#[test]
fn a_target_holding_the_same_data_is_taken_as_the_copy() {
    let m = Machine::new();
    let p = m.claude();
    cp_a(&p, &m.harness().join("claude/projects"));
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(is_link(&p));
}

/// A copy that keeps modes, times and links, as `ai harness` makes one.
fn cp_a(from: &Path, to: &Path) {
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    let status = Command::new("cp")
        .args(["-a", "--"])
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn a_copy_cut_off_is_thrown_away_and_redone() {
    let m = Machine::new();
    let p = m.claude();
    write(
        &m.harness()
            .join("claude/projects.incoming/-work/half.jsonl"),
        "{",
    );
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(is_link(&p));
    assert!(!m.harness().join("claude/projects.incoming").exists());
    assert!(
        !m.harness()
            .join("claude/projects/-work/half.jsonl")
            .exists()
    );
}

#[test]
fn a_move_cut_short_before_its_link_is_finished() {
    let m = Machine::new();
    let p = m.claude();
    cp_a(&p, &m.harness().join("claude/projects"));
    fs::rename(&p, backup(&p)).unwrap();
    assert_eq!(m.state("projects"), State::CutShort(p.clone()));
    assert!(m.run(harness::link).out.contains("cut short"));
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(is_link(&p));
    let record = Record::load(&m.harness().join("claude/projects.move.json"))
        .unwrap()
        .unwrap();
    assert!(record.verified);
}

#[test]
fn a_move_cut_short_onto_a_different_copy_needs_the_user() {
    let m = Machine::new();
    let p = m.claude();
    write(&m.harness().join("claude/projects/other.jsonl"), "x");
    fs::rename(&p, backup(&p)).unwrap();
    let r = m.run(harness::run);
    assert!(r.failed);
    assert!(r.err.contains("resolve it by hand"), "{}", r.err);
    assert!(!p.exists());
}

#[test]
fn a_link_that_reads_back_differently_is_undone() {
    let m = Machine::new();
    let p = m.claude();
    assert!(!m.run(harness::run).failed);
    // As if the move was stopped before reading back, and the copy then differed.
    let rec = m.harness().join("claude/projects.move.json");
    let mut record = Record::load(&rec).unwrap().unwrap();
    record.verified = false;
    fs::write(&rec, serde_json::to_string(&record).unwrap()).unwrap();
    fs::write(
        m.harness().join("claude/projects/-work/s1.jsonl"),
        "changed",
    )
    .unwrap();
    let r = m.run(harness::run);
    assert!(r.failed);
    assert!(r.err.contains("put back as it was"), "{}", r.err);
    assert!(!is_link(&p));
    assert_eq!(read(&p.join("-work/s1.jsonl")), "{\"a\":1}\n");
    assert!(!backup(&p).exists());
    assert!(!rec.exists());
}

#[test]
fn a_backup_goes_after_14_days_untouched() {
    let mut m = Machine::new();
    let p = m.claude();
    assert!(!m.run(harness::run).failed);
    m.now += SignedDuration::from_hours(13 * 24);
    assert!(!m.run(harness::run).failed);
    assert!(backup(&p).exists());
    m.now += SignedDuration::from_hours(2 * 24);
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(r.out.contains("Deleted"), "{}", r.out);
    assert!(!backup(&p).exists());
    assert!(!m.harness().join("claude/projects.move.json").exists());
    assert_eq!(read(&p.join("-work/s1.jsonl")), "{\"a\":1}\n");
}

#[test]
fn a_backup_written_to_after_its_move_is_kept() {
    let mut m = Machine::new();
    let p = m.claude();
    assert!(!m.run(harness::run).failed);
    fs::write(
        backup(&p).join("-work/s1.jsonl"),
        "{\"a\":1}\n{\"late\":2}\n",
    )
    .unwrap();
    m.now += SignedDuration::from_hours(15 * 24);
    let r = m.run(harness::run);
    assert!(r.failed);
    assert!(r.err.contains("something changed it"), "{}", r.err);
    assert!(backup(&p).exists());
}

#[test]
fn prune_does_not_wait() {
    let m = Machine::new();
    let p = m.claude();
    assert!(!m.run(harness::run).failed);
    let r = m.run(harness::prune);
    assert!(!r.failed, "{}", r.err);
    assert!(!backup(&p).exists());
}

#[test]
fn a_backup_beside_a_real_folder_needs_the_user() {
    let m = Machine::new();
    let p = m.claude();
    assert!(!m.run(harness::run).failed);
    // As if a harness had replaced its link with a folder of its own.
    fs::remove_file(&p).unwrap();
    write(&p.join("-work/s2.jsonl"), "{}\n");
    let r = m.run(harness::run);
    assert!(r.failed);
    assert!(r.err.contains("left from an earlier move"), "{}", r.err);
    assert!(r.err.contains("is not linked"), "{}", r.err);
}

#[test]
fn a_harness_folder_inside_ai_harness_is_left_alone() {
    let mut m = Machine::new();
    let own = m.harness().join("claude/agents/dev");
    write(&own.join("projects/-work/s9.jsonl"), "{}\n");
    m.vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        own.to_string_lossy().into_owned(),
    );
    assert!(!m.run(harness::link).failed);
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(!is_link(&own.join("projects")));
    assert_eq!(
        m.state("projects"),
        State::Inside(own.canonicalize().unwrap().join("projects"))
    );
}

#[test]
fn a_harness_variable_names_its_folder() {
    let mut m = Machine::new();
    let codex = m.root.join("elsewhere/codex");
    write(&codex.join("sessions/r.jsonl"), "{}\n");
    m.vars
        .insert("CODEX_HOME".into(), codex.to_string_lossy().into_owned());
    m.vars
        .insert("PI_CODING_AGENT_DIR".into(), "relative/pi".into());
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(is_link(&codex.join("sessions")));
    assert_eq!(read(&m.harness().join("codex/sessions/r.jsonl")), "{}\n");
    assert!(is_link(&m.home().join(".pi/agent/sessions")));
    assert!(!m.home().join("relative").exists());
}

#[test]
fn a_link_elsewhere_is_left_alone() {
    let m = Machine::new();
    let other = m.root.join("other");
    fs::create_dir_all(&other).unwrap();
    fs::create_dir_all(m.home().join(".claude")).unwrap();
    symlink(&other, m.home().join(".claude/projects")).unwrap();
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(r.err.contains("left alone"), "{}", r.err);
    assert_eq!(
        fs::read_link(m.home().join(".claude/projects")).unwrap(),
        other
    );
}

#[test]
fn the_link_goes_where_the_folder_really_sits() {
    let m = Machine::new();
    let config = m.root.join("config/claude");
    write(&config.join("projects/-work/s1.jsonl"), "{}\n");
    symlink(&config, m.home().join(".claude")).unwrap();
    let r = m.run(harness::run);
    assert!(!r.failed, "{}", r.err);
    assert!(is_link(&config.join("projects")));
    assert!(backup(&config.join("projects")).exists());
    assert_eq!(
        read(&m.home().join(".claude/projects/-work/s1.jsonl")),
        "{}\n"
    );
}

#[test]
fn no_room_moves_nothing() {
    let mut m = Machine::new();
    m.room = 0;
    let p = m.claude();
    let r = m.run(harness::run);
    assert!(r.failed);
    assert!(r.err.contains("nothing moved"), "{}", r.err);
    assert!(!is_link(&p));
}

#[test]
fn no_process_list_moves_nothing() {
    let m = Machine::new();
    fs::remove_dir(m.root.join("proc/self")).unwrap();
    let p = m.claude();
    let r = m.run(harness::run);
    assert!(r.failed);
    assert!(r.err.contains("lists no processes"), "{}", r.err);
    assert!(!is_link(&p));
}

#[test]
fn status_shows_each_place() {
    let m = Machine::new();
    let p = m.claude();
    let r = m.run(harness::status);
    assert!(r.out.contains("not moved"), "{}", r.out);
    assert!(m.run(harness::run).out.contains("Moved"));
    let r = m.run(harness::status);
    assert!(r.out.contains("~/.claude/projects"), "{}", r.out);
    assert!(r.out.contains("linked"), "{}", r.out);
    assert!(
        r.out.contains(&format!("backup {}", backup(&p).display())),
        "{}",
        r.out
    );
    assert!(r.out.contains("~/.hermes/state.db"), "{}", r.out);
}

#[test]
fn the_cli_links_and_reports() {
    let m = Machine::new();
    let ai = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ai"))
            .args(args)
            .env("HOME", m.home())
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("DSH_HOME")
            .env_remove("GEMINI_CLI_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("HERMES_HOME")
            .output()
            .unwrap()
    };
    let o = ai(&["harness", "link"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(is_link(&m.home().join(".claude/projects")));
    let o = ai(&["harness", "status"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("linked"));
}

#[cfg(target_os = "linux")]
#[test]
fn the_real_process_list_shows_a_process_working_in_a_folder() {
    let m = Machine::new();
    let dir = m.root.join("busy");
    fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new("sleep")
        .arg("30")
        .current_dir(&dir)
        .spawn()
        .unwrap();
    let none = regex::Regex::new("^$").unwrap();
    let found = harness::blockers(Path::new("/proc"), &none, std::slice::from_ref(&dir));
    child.kill().unwrap();
    child.wait().unwrap();
    let found = found.unwrap();
    assert!(found.iter().any(|p| p.pid == child.id()), "{found:?}");
    let idle = harness::blockers(Path::new("/proc"), &none, &[m.root.join("idle")]).unwrap();
    assert!(idle.iter().all(|p| p.pid != child.id()));
}
