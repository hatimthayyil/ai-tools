use std::{
    fs,
    path::{Path, PathBuf},
};

use super::{Options, Outcome, Report, convert, describe, kept, skipped};
use ai_tools::{
    agent::{Store, Synced},
    config::Homes,
    project::Project,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude")
        .join(name)
}

/// Every file under `dir` with its bytes, by its path there.
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            match p.is_dir() {
                true => walk(root, &p, out),
                false => out.push((p.strip_prefix(root).unwrap().into(), fs::read(&p).unwrap())),
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Writes `bytes` at `p` as a format-4 store kept a file: a directory of segments, each cut at the
/// first line's end past `cap` bytes.
fn segmented(p: &Path, bytes: &[u8], cap: usize) {
    fs::create_dir_all(p).unwrap();
    let ext = p
        .extension()
        .map_or(String::new(), |e| format!(".{}", e.to_string_lossy()));
    let mut parts = vec![Vec::new()];
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        let last = parts.last_mut().unwrap();
        if last.len() > cap {
            parts.push(Vec::new());
        }
        parts.last_mut().unwrap().extend_from_slice(line);
    }
    for (i, part) in parts.iter().enumerate() {
        fs::write(p.join(format!("{i:06}{ext}")), part).unwrap();
    }
}

/// The segments of the format-4 file `p`, joined.
fn joined(p: &Path) -> Vec<u8> {
    let mut names: Vec<PathBuf> = fs::read_dir(p)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    names.iter().flat_map(|s| fs::read(s).unwrap()).collect()
}

fn rows(p: &Path) -> Vec<Value> {
    String::from_utf8(fs::read(p).unwrap())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A project `repo` whose agents an older ai kept in `.ai/local` (format 4): agent dev with
/// sessions s-1 and s-2, s-2 since detached and attached to agent other; private settings and
/// evals beside them; the transcripts in a Claude Code home of the temporary directory.
struct Old {
    dir: TempDir,
}

impl Old {
    fn new() -> Old {
        let o = Old {
            dir: TempDir::new().unwrap(),
        };
        let projects = o.dir.path().join("claude/projects/-x");
        fs::create_dir_all(projects.join("s-1/tool-results")).unwrap();
        fs::copy(fixture("plain.jsonl"), o.s1()).unwrap();
        fs::copy(fixture("tools.jsonl"), o.s2()).unwrap();
        fs::write(o.artifact(), b"tool\0output").unwrap();

        let build = o.dir.path().join("build");
        fs::create_dir_all(&build).unwrap();
        let built = Homes::at(&o.dir.path().join("build-home"));
        Project::init_in(&build, &built).unwrap();
        let store = Store::find_in(&built, &build).unwrap().unwrap();
        store.create("dev").unwrap();
        store.create("other").unwrap();
        store.attach("dev", "claude", "s-1", Some(&o.s1())).unwrap();
        store.attach("dev", "claude", "s-2", Some(&o.s2())).unwrap();
        store.detach("claude", "s-2").unwrap();
        store
            .attach("other", "claude", "s-2", Some(&o.s2()))
            .unwrap();

        fs::create_dir_all(o.repo()).unwrap();
        fs::write(
            o.repo().join("ai.toml"),
            format!(
                "[project]\nid = \"{}\"\n",
                Project::find(&build).unwrap().unwrap().id.replace('8', "9")
            ),
        )
        .unwrap();
        let local = o.local();
        gix::init(&local).unwrap();
        fs::write(local.join(".gitignore"), "*.lock\n*.log\n").unwrap();
        fs::write(local.join("ai.toml"), "[summariser]\nmodel = \"opus\"\n").unwrap();
        fs::create_dir_all(local.join("evals/run")).unwrap();
        fs::write(local.join("evals/run/result.json"), "{}\n").unwrap();
        let agents = local.join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("format"), "4\n").unwrap();
        fs::write(agents.join("dev.log"), "a log\n").unwrap();
        let mut events = Vec::new();
        let threads = built.repo.join("projects/build/agents/threads");
        for name in ["dev", "other"] {
            let (from, to) = (threads.join(name), agents.join("threads").join(name));
            let items = fs::read(from.join("items.jsonl")).unwrap();
            segmented(&to.join("items.jsonl"), &items, 512);
            let mut tracks = Vec::new();
            for mut t in rows(&from.join("sessions.jsonl")) {
                t.as_object_mut().unwrap().remove("config");
                serde_json::to_writer(&mut tracks, &t).unwrap();
                tracks.push(b'\n');
            }
            segmented(&to.join("sessions.jsonl"), &tracks, 512);
            for mut e in rows(&from.join("attachments.jsonl")) {
                if e["state"] == "attached" {
                    e["thread"] = json!(name);
                }
                events.push(e);
            }
            for p in rows(&from.join("sources.jsonl")) {
                let raw = Path::new(p["path"].as_str().unwrap());
                let rel = raw.strip_prefix(&projects).unwrap();
                let copy = to
                    .join("sources/claude")
                    .join(p["session"].as_str().unwrap())
                    .join(rel);
                segmented(&copy, &fs::read(raw).unwrap(), 512);
            }
        }
        events.sort_by_key(|e| e["at"].as_str().unwrap().to_string());
        let mut all = Vec::new();
        for e in &events {
            serde_json::to_writer(&mut all, e).unwrap();
            all.push(b'\n');
        }
        segmented(&agents.join("sessions.jsonl"), &all, 512);
        o
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn local(&self) -> PathBuf {
        self.repo().join(".ai/local")
    }

    fn homes(&self) -> Homes {
        Homes::at(&self.dir.path().join("home"))
    }

    fn s1(&self) -> PathBuf {
        self.dir.path().join("claude/projects/-x/s-1.jsonl")
    }

    fn s2(&self) -> PathBuf {
        self.dir.path().join("claude/projects/-x/s-2.jsonl")
    }

    fn artifact(&self) -> PathBuf {
        self.dir
            .path()
            .join("claude/projects/-x/s-1/tool-results/out.bin")
    }

    fn project(&self) -> Project {
        Project::find(&self.repo()).unwrap().unwrap()
    }

    fn store(&self) -> Store {
        Store::find_in(&self.homes(), &self.repo())
            .unwrap()
            .unwrap()
    }
}

impl Old {
    fn convert(&self, dry_run: bool) -> anyhow::Result<Outcome> {
        convert(&self.repo(), &self.homes(), Options { dry_run })
    }

    fn report(&self) -> Report {
        match self.convert(false).unwrap() {
            Outcome::Converted(r) => r,
            o => panic!("not converted: {o:?}"),
        }
    }

    fn folder(&self) -> PathBuf {
        self.homes().repo.join("projects/repo")
    }

    /// Every file under the old store, the user's homes and the harness's folder.
    fn everything(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for d in [
            self.local(),
            self.dir.path().join("home"),
            self.dir.path().join("claude"),
        ] {
            if d.exists() {
                out.extend(files(&d).into_iter().map(|(p, b)| (d.join(p), b)));
            }
        }
        out
    }
}

/// The subject of the newest commit of the repository at `dir`.
fn head_summary(dir: &Path) -> String {
    gix::open(dir)
        .unwrap()
        .head_commit()
        .unwrap()
        .message()
        .unwrap()
        .summary()
        .to_string()
}

#[test]
fn ai_refuses_a_store_awaiting_conversion_and_names_the_converter() {
    let o = Old::new();
    let e = Store::find_in(&o.homes(), &o.repo()).err().unwrap();
    assert!(e.to_string().contains("ai convert"), "{e}");
    o.report();
    Store::find_in(&o.homes(), &o.repo()).unwrap().unwrap();
}

#[test]
fn a_conversion_keeps_every_thread_and_points_at_the_harness_files() {
    let o = Old::new();
    let old_items = |t: &str| joined(&o.local().join("agents/threads").join(t).join("items.jsonl"));
    let (dev_items, other_items) = (old_items("dev"), old_items("other"));
    let old = files(&o.local());
    let s2 = fs::read(o.s2()).unwrap();
    fs::write(o.s2(), &s2[..s2.len() / 2]).unwrap();
    fs::remove_file(o.artifact()).unwrap();

    let r = o.report();

    assert_eq!(r.threads.len(), 2);
    assert!(r.settings);
    let restored_s2 = o
        .dir
        .path()
        .join("claude/projects/-x/s-2/restored/s-2.jsonl");
    assert_eq!(
        fs::read(&restored_s2).unwrap(),
        s2,
        "a cut transcript's copy is restored"
    );
    assert_eq!(
        fs::read(o.artifact()).unwrap(),
        b"tool\0output",
        "a gone file's copy is restored where it was"
    );
    let mut to: Vec<&Path> = r.restored.iter().map(|t| t.to.as_path()).collect();
    to.sort();
    to.dedup();
    assert_eq!(to, [o.artifact().as_path(), restored_s2.as_path()]);
    assert_eq!(r.pointed, 1, "s-1 is still whole in the harness's files");

    assert_eq!(files(&o.local()), old, "the old store is untouched");
    let folder = o.folder();
    assert_eq!(
        fs::read_to_string(folder.join("ai.toml")).unwrap(),
        "[summariser]\nmodel = \"opus\"\n"
    );
    assert!(
        !folder.join("evals").exists(),
        "the project's working data stays in .ai/local"
    );
    assert_eq!(
        fs::read_link(o.repo().join("ai.local.toml")).unwrap(),
        folder.join("ai.toml")
    );
    assert!(!folder.join("agents/threads/dev/sources").exists());
    assert!(!folder.join(".git").exists() && !folder.join(".gitignore").exists());
    assert_eq!(
        head_summary(&o.homes().repo),
        "ai convert: repo's agents and private settings from .ai/local"
    );
    let building = o.homes().repo.join(".git/ai-convert");
    assert_eq!(
        fs::read_dir(&building).unwrap().count(),
        0,
        "nothing left building"
    );

    let store = o.store();
    let thread = |t: &str| store.thread(t).unwrap().snapshot().unwrap();
    let new_items =
        |t: &str| fs::read(folder.join("agents/threads").join(t).join("items.jsonl")).unwrap();
    assert_eq!(
        (new_items("dev"), new_items("other")),
        (dev_items, other_items)
    );
    for t in ["dev", "other"] {
        let snap = thread(t);
        let old: Vec<Value> = String::from_utf8(old_items(t))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let new = snap.heads().unwrap();
        assert_eq!(new.len(), old.len(), "{t}: every item");
        assert_eq!(
            snap.items().unwrap().len(),
            old.len(),
            "{t}: every item reads"
        );
        for (n, o) in new.iter().zip(&old) {
            assert_eq!(n.seq, o["seq"].as_u64().unwrap(), "{t}: item by item");
        }
    }
    let state = |t: &str| {
        let mut b: Vec<(String, bool, Option<PathBuf>)> = thread(t)
            .bindings()
            .unwrap()
            .into_iter()
            .map(|b| (b.session.clone(), b.is_attached(), b.transcript))
            .collect();
        b.sort();
        b
    };
    assert_eq!(
        state("dev"),
        [
            ("s-1".into(), true, Some(o.s1())),
            ("s-2".into(), false, Some(o.s2()))
        ]
    );
    assert_eq!(state("other"), [("s-2".into(), true, Some(o.s2()))]);
    let config = |t: &str, s: &str| thread(t).binding("claude", s).unwrap().unwrap().config;
    assert_eq!(
        (config("dev", "s-1"), config("other", "s-2")),
        (
            Some(o.dir.path().join("claude")),
            Some(o.dir.path().join("claude"))
        ),
        "each session's configuration directory is its transcript's"
    );

    for s in ["s-1", "s-2"] {
        assert!(matches!(
            store.sync("claude", s, None).unwrap(),
            Synced::Attached { added, .. } if added.is_empty()
        ));
    }
}

#[test]
fn a_dry_run_changes_nothing_and_tells_what_would_happen() {
    let o = Old::new();
    fs::remove_file(o.artifact()).unwrap();
    let before = o.everything();
    let r = match o.convert(true).unwrap() {
        Outcome::Converted(r) => r,
        x => panic!("{x:?}"),
    };
    assert!(r.dry_run);
    assert_eq!(r.threads.len(), 2);
    assert_eq!(
        r.restored.iter().map(|t| t.to.clone()).collect::<Vec<_>>(),
        [o.artifact()]
    );
    assert_eq!(o.everything(), before, "nothing changed");
    assert!(!o.homes().repo.exists() && !o.homes().machine.exists());
    let stay = kept(2, r.pointed, true);
    let said = describe(&o.repo(), &Outcome::Converted(r)).join("\n");
    let first = said.lines().next().unwrap();
    assert!(
        first.contains("would convert 2 agents") && first.ends_with(&format!(". {stay}")),
        "{said}"
    );
    assert!(said.contains("Would restore"), "{said}");
}

#[test]
fn the_report_tells_how_many_session_files_stay_where_the_harness_keeps_them() {
    assert_eq!(
        kept(5, 250, false),
        "Their 250 session files stay where the harness keeps them."
    );
    assert_eq!(
        kept(1, 1, false),
        "Its only session file stays where the harness keeps it."
    );
    assert_eq!(
        kept(2, 1, true),
        "One of their session files stays where the harness keeps it."
    );
    assert_eq!(
        kept(1, 3, true),
        "3 of its session files stay where the harness keeps them."
    );
}

#[test]
fn a_second_run_says_it_is_done_and_changes_nothing() {
    let o = Old::new();
    o.report();
    let before = o.everything();
    let again = o.convert(false).unwrap();
    assert_eq!(
        again,
        Outcome::Done {
            project: "repo".into(),
            folder: o.folder()
        }
    );
    assert_eq!(o.everything(), before);
}

#[test]
fn a_conversion_cut_short_before_its_move_starts_again_from_nothing() {
    let o = Old::new();
    gix::init(&o.homes().repo).unwrap();
    let building = o.homes().repo.join(".git/ai-convert");
    let left = building.join(format!("{}-cut", o.project().id));
    fs::create_dir_all(left.join("projects/repo/agents")).unwrap();
    fs::write(left.join("projects/repo/agents/format"), "6\n").unwrap();

    o.report();

    assert!(!left.exists(), "what the cut run built is swept");
    assert_eq!(o.store().threads().unwrap().len(), 2);
}

#[test]
fn a_conversion_cut_short_after_its_move_is_finished_by_the_next_run() {
    let o = Old::new();
    o.report();
    let link = o.repo().join("ai.local.toml");
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(o.local().join("ai.toml"), &link).unwrap();
    let repo = &o.homes().repo;
    let before = head_summary(repo);

    assert!(matches!(o.convert(false).unwrap(), Outcome::Done { .. }));

    assert_eq!(fs::read_link(&link).unwrap(), o.folder().join("ai.toml"));
    assert_eq!(head_summary(repo), before);
}

#[test]
fn a_folder_another_checkout_converted_is_not_overwritten() {
    let o = Old::new();
    o.report();
    fs::remove_dir_all(o.homes().machine.join("converted")).unwrap();
    let before = o.everything();
    let e = o.convert(false).unwrap_err();
    assert!(
        e.to_string()
            .contains("already holds this project's agents"),
        "{e}"
    );
    assert_eq!(o.everything(), before);
}

#[test]
fn a_folder_name_another_project_holds_is_refused() {
    let o = Old::new();
    let taken = o.homes().repo.join("projects/repo");
    fs::create_dir_all(&taken).unwrap();
    fs::write(taken.join("id"), "project_01h455vb4pex5vsknk084sn02q\n").unwrap();
    let before = o.everything();
    assert!(o.convert(false).is_err());
    assert_eq!(o.everything(), before);
}

#[test]
fn a_store_of_another_format_is_not_converted() {
    let o = Old::new();
    fs::write(o.local().join("agents/format"), "5\n").unwrap();
    let e = o.convert(false).unwrap_err();
    assert!(
        e.to_string()
            .contains("its agents are format 5; this converts format 4"),
        "{e}"
    );
    assert!(!o.folder().exists());
}

#[test]
fn a_conversion_joins_each_segmented_file_into_one_of_the_same_bytes() {
    let o = Old::new();
    let dev = o.local().join("agents/threads/dev");
    let (mut lines, mut agents) = (Vec::new(), Vec::new());
    for i in 0..40 {
        lines.extend_from_slice(b"{\"op\":\"clear\"}\n");
        agents.extend_from_slice(
            format!(
                "{{\"harness\":\"claude\",\"agent\":\"a{i}\",\"agent_type\":\"t\",\"description\":\"d\",\"call\":null}}\n"
            )
            .as_bytes(),
        );
    }
    lines.extend_from_slice(b"{\"torn");
    segmented(&dev.join("lines.jsonl"), &lines, 100);
    segmented(&dev.join("agents.jsonl"), &agents, 100);
    segmented(
        &dev.join("agents/claude/a1.jsonl"),
        &lines[..lines.len() - 6],
        64,
    );
    let mut old = Vec::new();
    for f in [
        "items.jsonl",
        "lines.jsonl",
        "agents.jsonl",
        "agents/claude/a1.jsonl",
    ] {
        let p = dev.join(f);
        assert!(fs::read_dir(&p).unwrap().count() > 1, "{f} has one segment");
        old.push((f, joined(&p)));
    }

    o.report();

    let new = o.folder().join("agents/threads/dev");
    for (f, bytes) in old {
        assert!(new.join(f).is_file(), "{f} is one file");
        assert_eq!(fs::read(new.join(f)).unwrap(), bytes, "{f}");
    }
}

#[test]
fn a_segment_out_of_order_stops_the_conversion() {
    let o = Old::new();
    let items = o.local().join("agents/threads/dev/items.jsonl");
    fs::remove_file(items.join("000000.jsonl")).unwrap();
    let e = o.convert(false).unwrap_err();
    assert!(
        format!("{e:#}").contains("its segments are not numbered from 0"),
        "{e:#}"
    );
    assert!(!o.folder().exists());
}

#[test]
fn a_session_the_old_store_attached_twice_ends_attached_to_the_later_thread() {
    let o = Old::new();
    let all = o.local().join("agents/sessions.jsonl");
    let mut kept = Vec::new();
    for line in joined(&all).split_inclusive(|&b| b == b'\n') {
        let e: Value = serde_json::from_slice(line).unwrap();
        if e["state"] != "detached" {
            kept.extend_from_slice(line);
        }
    }
    fs::remove_dir_all(&all).unwrap();
    segmented(&all, &kept, 512);

    o.report();

    let store = o.store();
    let attached = |t: &str| {
        let snap = store.thread(t).unwrap().snapshot().unwrap();
        let mut b: Vec<(String, bool)> = snap
            .bindings()
            .unwrap()
            .into_iter()
            .map(|b| (b.session.clone(), b.is_attached()))
            .collect();
        b.sort();
        b
    };
    assert_eq!(
        attached("dev"),
        [("s-1".into(), true), ("s-2".into(), false)]
    );
    assert_eq!(attached("other"), [("s-2".into(), true)]);
    assert!(matches!(
        store.sync("claude", "s-2", None).unwrap(),
        Synced::Attached { thread, .. } if thread == "other"
    ));
}

#[test]
fn settings_alone_are_converted_and_a_project_without_either_is_skipped() {
    let o = Old::new();
    fs::remove_dir_all(o.local().join("agents")).unwrap();
    let r = o.report();
    assert!(r.threads.is_empty() && r.settings);
    assert_eq!(
        fs::read_to_string(o.folder().join("ai.toml")).unwrap(),
        "[summariser]\nmodel = \"opus\"\n"
    );

    let p = Old::new();
    fs::remove_dir_all(p.local().join("agents")).unwrap();
    fs::write(p.local().join("ai.toml"), "").unwrap();
    assert_eq!(
        p.convert(false).unwrap(),
        Outcome::Nothing {
            project: "repo".into()
        }
    );
    assert!(!p.homes().repo.exists());
}

#[test]
fn a_run_over_several_directories_counts_what_it_skipped_in_one_line() {
    assert_eq!(skipped(0, 0), None);
    assert_eq!(
        skipped(1, 0).unwrap(),
        "Skipped 1 directory that holds no ai project."
    );
    assert_eq!(
        skipped(0, 1).unwrap(),
        "Skipped 1 project with nothing to convert in .ai/local."
    );
    assert_eq!(
        skipped(37, 4).unwrap(),
        "Skipped 37 directories that hold no ai project, and 4 projects with nothing to convert \
         in .ai/local."
    );
}
