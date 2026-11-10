use std::{
    fs,
    path::{Path, PathBuf},
};

use ai_tools::{
    agent::{Store, Synced, convert},
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

/// Copies the directory `from` to `to`, everything in it.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dst = to.join(p.file_name().unwrap());
        match p.is_dir() {
            true => copy_dir(&p, &dst),
            false => {
                fs::copy(&p, &dst).unwrap();
            }
        }
    }
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
            for f in ["items.jsonl", "sessions.jsonl"] {
                segmented(&to.join(f), &fs::read(from.join(f)).unwrap(), 512);
            }
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

#[test]
fn an_older_ais_store_is_refused_until_it_moves() {
    let o = Old::new();
    let before = files(&o.local());
    let e = Store::find_in(&o.homes(), &o.repo()).err().unwrap();
    assert!(e.to_string().contains("where an older ai kept them"), "{e}");
    let declined = convert::offer_with(&o.project(), &o.homes(), |q| {
        assert!(q.contains(".ai/local") && q.contains("archived"), "{q}");
        Ok(false)
    })
    .unwrap_err();
    assert!(declined.to_string().starts_with("Not moved"), "{declined}");
    assert_eq!(files(&o.local()), before, "declining changes nothing");
    assert!(!o.homes().machine.join("archive").exists());
}

#[test]
fn a_move_keeps_every_thread_and_points_at_the_harness_files() {
    let o = Old::new();
    let old_items = |t: &str| joined(&o.local().join("agents/threads").join(t).join("items.jsonl"));
    let (dev_items, other_items) = (old_items("dev"), old_items("other"));
    let old = files(&o.local());
    let s2 = fs::read(o.s2()).unwrap();
    fs::write(o.s2(), &s2[..s2.len() / 2]).unwrap();
    fs::remove_file(o.artifact()).unwrap();

    let moved = convert::convert(&o.project(), &o.homes()).unwrap();

    assert_eq!(moved.threads, 2);
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
    let mut to: Vec<&Path> = moved.restored.iter().map(|(_, to)| to.as_path()).collect();
    to.sort();
    to.dedup();
    assert_eq!(to, [o.artifact().as_path(), restored_s2.as_path()]);
    assert_eq!(
        moved.pointed, 1,
        "s-1 is still whole in the harness's files"
    );

    assert!(
        !o.local().exists(),
        "the old store is gone from the project"
    );
    let archives: Vec<PathBuf> =
        fs::read_dir(o.homes().machine.join("archive").join(&o.project().id))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
    assert_eq!(archives, std::slice::from_ref(&moved.archive));
    assert!(moved.archive.to_str().unwrap().ends_with("--local"));
    assert_eq!(
        files(&moved.archive),
        old,
        "the archive is the old store, byte for byte"
    );

    let folder = o.homes().repo.join("projects/repo");
    assert_eq!(
        fs::read_to_string(folder.join("ai.toml")).unwrap(),
        "[summariser]\nmodel = \"opus\"\n"
    );
    assert_eq!(
        fs::read_to_string(folder.join("evals/run/result.json")).unwrap(),
        "{}\n"
    );
    assert_eq!(
        fs::read_link(o.repo().join("ai.local.toml")).unwrap(),
        folder.join("ai.toml")
    );
    assert!(!folder.join("agents/threads/dev/sources").exists());
    assert!(!folder.join(".git").exists() && !folder.join(".gitignore").exists());
    let log = gix::open(&o.homes().repo)
        .unwrap()
        .head_commit()
        .unwrap()
        .message()
        .unwrap()
        .summary()
        .to_string();
    assert_eq!(log, "ai: move repo's agents from .ai/local");

    let store = o.store();
    let thread = |t: &str| store.thread(t).unwrap().snapshot().unwrap();
    let new_items =
        |t: &str| fs::read(folder.join("agents/threads").join(t).join("items.jsonl")).unwrap();
    assert_eq!(
        (new_items("dev"), new_items("other")),
        (dev_items, other_items)
    );
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

    for s in ["s-1", "s-2"] {
        assert!(matches!(
            store.sync("claude", s, None).unwrap(),
            Synced::Attached { added, .. } if added.is_empty()
        ));
    }
}

#[test]
fn a_move_cut_short_after_its_store_is_in_place_is_finished() {
    let o = Old::new();
    let first = convert::convert(&o.project(), &o.homes()).unwrap();
    let archive = o.dir.path().join("kept");
    copy_dir(&first.archive, &archive);
    copy_dir(&archive, &o.local());
    let again = convert::convert(&o.project(), &o.homes()).unwrap();
    assert_eq!(
        again.archive, first.archive,
        "the archive of the same store is reused"
    );
    assert!(!o.local().exists());
    assert_eq!(o.store().threads().unwrap().len(), 2);
}

#[test]
fn a_store_of_another_format_is_not_moved() {
    let o = Old::new();
    fs::write(o.local().join("agents/format"), "5\n").unwrap();
    let e = convert::convert(&o.project(), &o.homes()).unwrap_err();
    assert!(
        e.to_string()
            .contains("its agents are format 5; this ai moves format 4"),
        "{e}"
    );
    assert!(o.local().join("agents/threads/dev").exists());
}

#[test]
fn a_move_joins_each_segmented_file_into_one_of_the_same_bytes() {
    let o = Old::new();
    let dev = o.local().join("agents/threads/dev");
    let mut lines = Vec::new();
    for i in 0..40 {
        lines.extend_from_slice(format!("{{\"op\":\"clear\",\"n\":{i}}}\n").as_bytes());
    }
    lines.extend_from_slice(b"{\"torn");
    segmented(&dev.join("lines.jsonl"), &lines, 100);
    segmented(&dev.join("agents.jsonl"), &lines[..lines.len() - 6], 100);
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

    convert::convert(&o.project(), &o.homes()).unwrap();

    let new = o.homes().repo.join("projects/repo/agents/threads/dev");
    for (f, bytes) in old {
        assert!(new.join(f).is_file(), "{f} is one file");
        assert_eq!(fs::read(new.join(f)).unwrap(), bytes, "{f}");
    }
}

#[test]
fn a_segment_out_of_order_stops_the_move() {
    let o = Old::new();
    let items = o.local().join("agents/threads/dev/items.jsonl");
    fs::remove_file(items.join("000000.jsonl")).unwrap();
    let e = convert::convert(&o.project(), &o.homes()).unwrap_err();
    assert!(
        format!("{e:#}").contains("its segments are not numbered from 0"),
        "{e:#}"
    );
    assert!(o.local().join("agents/threads/dev").exists());
}
