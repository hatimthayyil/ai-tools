use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use ai_tools::{
    agent::{Item, Key, Kind, LineKind, Store, Synced, Tree, claude, read, sync},
    backup::host,
    config::Homes,
    file::{human, pretty, sha256},
    memory::Who,
    project::Project,
};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude")
        .join(name)
}

/// The lines of a fixture, each with its newline.
fn lines(name: &str) -> Vec<String> {
    fs::read_to_string(fixture(name))
        .unwrap()
        .split_inclusive('\n')
        .map(String::from)
        .collect()
}

/// `lines` with every entry id prefixed by `p`: fixtures share ids, Claude Code sessions never do.
fn relabel(lines: &[String], p: &str) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            for k in ["uuid", "parentUuid", "logicalParentUuid"] {
                if let Some(id) = v[k].as_str() {
                    v[k] = format!("{p}{id}").into();
                }
            }
            format!("{v}\n")
        })
        .collect()
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_in(dir, args, "")
}

/// `git args` in `dir` with `input` on stdin; its stdout.
fn git_in(dir: &Path, args: &[&str], input: &str) -> String {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A git repository in a temporary directory whose committed `ai.toml` makes it a project, with
/// agent dev, and transcripts beside it.
struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Repo {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        gix::init(&repo).unwrap();
        Project::init_in(&repo, &homes_of(&repo)).unwrap();
        git(&repo, &["add", "ai.toml"]);
        git(&repo, &["commit", "-q", "-m", "root"]);
        let r = Repo { dir };
        r.store().create("dev").unwrap();
        r
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn store(&self) -> Store {
        Store::find_in(&self.homes(), &self.path())
            .unwrap()
            .unwrap()
    }

    /// Writes the transcript of `session`.
    fn transcript(&self, session: &str, lines: &[String]) -> PathBuf {
        let p = self.dir.path().join(format!("{session}.jsonl"));
        fs::write(&p, lines.concat()).unwrap();
        p
    }

    /// The files of thread `name`, by path within it, its repository and locks left out; `None`
    /// before it exists.
    fn state(&self, name: &str) -> Option<BTreeMap<PathBuf, Vec<u8>>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.ends_with(".git") || p.extension().is_some_and(|x| x == "lock") {
                    continue;
                }
                match p.is_dir() {
                    true => walk(root, &p, out),
                    false => {
                        out.insert(p.strip_prefix(root).unwrap().into(), fs::read(&p).unwrap());
                    }
                }
            }
        }
        let dir = self.store_dir().join("threads").join(name);
        dir.is_dir().then(|| {
            let mut out = BTreeMap::new();
            walk(&dir, &dir, &mut out);
            out
        })
    }

    /// The agent store of the project, in its folder of the user's repository.
    fn store_dir(&self) -> PathBuf {
        self.homes().repo.join("projects/repo/agents")
    }

    /// The user's homes, beside the project.
    fn homes(&self) -> Homes {
        homes_of(&self.path())
    }

    /// This machine's files for the project's agents: their locks and logs.
    fn local_dir(&self) -> PathBuf {
        let id = Project::find(&self.path()).unwrap().unwrap().id;
        self.homes().machine.join("agents").join(id)
    }
}

/// The user's homes for the project at `repo`: beside it, never the real ones.
fn homes_of(repo: &Path) -> Homes {
    Homes::at(&repo.parent().unwrap().join("home"))
}

fn entries(items: &[Item]) -> Vec<String> {
    items.iter().map(|i| i.id.entry.clone()).collect()
}

/// Each item as `entry<-parent entry`.
fn links(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .map(|i| {
            let p = i.parent.as_ref().map_or("", |p| p.entry.as_str());
            format!("{}<-{p}", i.id.entry)
        })
        .collect()
}

fn added(s: Synced) -> std::ops::Range<u64> {
    match s {
        Synced::Attached { added, .. } => added.items,
        Synced::NotAttached => panic!("not attached"),
    }
}

/// The file `p` as text.
fn text(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

/// The newest version of `file` that thread `thread` read: the length and SHA-256 its records
/// tell of.
fn source(r: &Repo, thread: &str, file: &Path) -> Option<(u64, String)> {
    let real = real(file);
    let snap = r.store().thread(thread).unwrap().snapshot().unwrap();
    snap.versions()
        .unwrap()
        .into_iter()
        .rfind(|v| v.read.path == real)
        .map(|v| (v.read.len, v.read.sha256))
}

/// `p` where it really sits: its folder's links resolved.
fn real(p: &Path) -> PathBuf {
    fs::canonicalize(p.parent().unwrap())
        .unwrap()
        .join(p.file_name().unwrap())
}

fn assert_source(r: &Repo, thread: &str, file: &Path, expected: &[u8]) {
    assert_eq!(
        source(r, thread, file),
        Some((expected.len() as u64, sha256(expected))),
        "the record of {}",
        file.display()
    );
}

#[test]
fn attach_ingests_a_session_and_sync_adds_only_new_entries() {
    let r = Repo::new();
    let all = lines("tools.jsonl");
    let path = r.transcript("s-1", &all[..8]);
    let store = r.store();
    let a = store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let first = a.added.items.clone();
    assert!(!first.is_empty() && first.start == 0);

    r.transcript("s-1", &all);
    let more = added(sync(&r.homes(), &r.path(), "claude", "s-1", None).unwrap());
    assert_eq!(more.start, first.end);
    let head = r.state("dev");
    let none = added(sync(&r.homes(), &r.path(), "claude", "s-1", Some(&path)).unwrap());
    assert!(none.is_empty());
    assert_eq!(r.state("dev"), head, "no commit when nothing is new");

    let want = claude::read(&path, "s-1").unwrap();
    let thread = store.thread("dev").unwrap();
    let s = thread.snapshot().unwrap();
    assert_eq!(s.len().unwrap(), more.end);
    assert_eq!(
        s.items().unwrap(),
        want,
        "items, sources, kinds and raw lines kept"
    );
    assert_eq!(s.item(3).unwrap(), want[3]);
    assert_eq!(s.tip("claude", "s-1").unwrap(), Some(more.end - 1));
    let tree = s.tree().unwrap();
    let parsed = Tree::new(want);
    assert_eq!(tree.tips(), parsed.tips());
    assert_eq!(tree.branch(tree.tips()[0]), parsed.branch(parsed.tips()[0]));

    let b = s.binding("claude", "s-1").unwrap().unwrap();
    assert_eq!((b.graft, b.transcript.as_deref()), (None, Some(&*path)));
    assert!(b.is_attached());
    assert_eq!(
        fs::read_to_string(r.store_dir().join("format")).unwrap(),
        "6\n"
    );
    let events = text(&r.store_dir().join("threads/dev/attachments.jsonl"));
    assert_eq!(events.lines().count(), 1, "one attachment: {events}");
    let rows = text(&r.store_dir().join("threads/dev/items.jsonl"));
    assert_eq!(rows.lines().count() as u64, more.end);
}

#[test]
fn an_unattached_session_is_not_attached() {
    let r = Repo::new();
    let outside = TempDir::new().unwrap();
    let s = sync(&r.homes(), outside.path(), "claude", "s-1", None).unwrap();
    assert_eq!(s, Synced::NotAttached, "outside a repository");
    assert_eq!(
        sync(&r.homes(), &r.path(), "claude", "s-1", None).unwrap(),
        Synced::NotAttached
    );
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let store = r.store();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    assert_eq!(
        sync(&r.homes(), &r.path(), "claude", "s-2", None).unwrap(),
        Synced::NotAttached
    );
    assert_eq!(store.detach("claude", "s-1").unwrap(), ["dev"]);
    assert_eq!(store.detach("claude", "s-1").unwrap(), [""; 0]);
    let head = r.state("dev");
    assert_eq!(
        sync(&r.homes(), &r.path(), "claude", "s-1", Some(&path)).unwrap(),
        Synced::NotAttached
    );
    assert_eq!(r.state("dev"), head);
    assert!(sync(&r.homes(), &r.path(), "claude", "../s-1", None).is_err());
}

#[test]
fn a_session_without_a_transcript_yet_is_bound_and_synced_later() {
    let r = Repo::new();
    let path = r.dir.path().join("s-1.jsonl");
    let a = r
        .store()
        .attach("dev", "claude", "s-1", Some(&path))
        .unwrap();
    assert!(a.added.is_empty());
    r.transcript("s-1", &lines("plain.jsonl"));
    assert_eq!(
        added(sync(&r.homes(), &r.path(), "claude", "s-1", None).unwrap()),
        0..4
    );
}

#[test]
fn a_second_session_grafts_onto_the_newest_item() {
    let r = Repo::new();
    let store = r.store();
    let p1 = r.transcript("s-1", &lines("plain.jsonl"));
    let p2 = r.transcript("s-2", &relabel(&lines("rewind.jsonl"), "b-"));
    store.attach("dev", "claude", "s-1", Some(&p1)).unwrap();
    let a = store.attach("dev", "claude", "s-2", Some(&p2)).unwrap();
    assert_eq!(a.added.items, 4..14);
    let t = store.thread("dev").unwrap();
    let s = t.snapshot().unwrap();
    let items = s.items().unwrap();
    assert_eq!(
        links(&items)[4],
        "b-u1<-a3",
        "the root hangs on s-1's newest item"
    );
    assert_eq!(items[4].id.session, "s-2");
    assert_eq!(items[3].id.session, "s-1");
    assert_eq!(s.binding("claude", "s-2").unwrap().unwrap().graft, Some(3));
    assert_eq!(s.tips().unwrap(), [9, 13]);
    assert_eq!(s.tip("claude", "s-1").unwrap(), Some(3));
    assert_eq!(s.tip("claude", "s-2").unwrap(), Some(13));
    let tree = s.tree().unwrap();
    assert_eq!(
        entries(&tree.path(13).into_iter().cloned().collect::<Vec<_>>()),
        [
            "u1", "a2", "u2", "a3", "b-u1", "b-a1", "b-u3", "b-a4", "b-r2", "b-a5"
        ]
    );
    let list = store.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "dev");
    assert_eq!(list[0].tips, [9, 13]);
    let newest = list[0].newest.as_ref().unwrap();
    assert_eq!(
        (newest.seq, newest.harness.as_str(), newest.session.as_str()),
        (13, "claude", "s-2")
    );
    assert_eq!(newest.ts, items[13].ts);
}

#[test]
fn a_fork_adds_only_its_new_entries() {
    let r = Repo::new();
    let store = r.store();
    let all = lines("rewind.jsonl");
    let p1 = r.transcript("s-1", &all[..6]);
    store.attach("dev", "claude", "s-1", Some(&p1)).unwrap();
    let n = store
        .thread("dev")
        .unwrap()
        .snapshot()
        .unwrap()
        .len()
        .unwrap();
    let forked: Vec<String> = all
        .iter()
        .map(|l| l.replace("3c4d5e6f", "99999999"))
        .collect();
    let p2 = r.transcript("s-2", &forked);
    let a = store.attach("dev", "claude", "s-2", Some(&p2)).unwrap();
    let t = store.thread("dev").unwrap();
    let s = t.snapshot().unwrap();
    let items = s.items().unwrap();
    let parsed = claude::read(&p2, "s-2").unwrap();
    assert_eq!(a.added.items, n..parsed.len() as u64);
    assert_eq!(
        entries(&items),
        entries(&parsed),
        "copied entries are not added again"
    );
    assert!(items[..n as usize].iter().all(|i| i.id.session == "s-1"));
    assert!(items[n as usize..].iter().all(|i| i.id.session == "s-2"));
    assert_eq!(
        links(&items),
        links(&parsed),
        "new entries hang on the copies"
    );
}

#[test]
fn detach_and_reattach_catch_up() {
    let r = Repo::new();
    let store = r.store();
    let all = lines("interject.jsonl");
    let p = r.transcript("s-1", &all[..6]);
    let other = r.transcript("s-0", &relabel(&lines("plain.jsonl"), "z-"));
    store.attach("dev", "claude", "s-0", Some(&other)).unwrap();
    store.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    store.detach("claude", "s-1").unwrap();
    let t = store.thread("dev").unwrap();
    let s = t.snapshot().unwrap();
    let b = s.binding("claude", "s-1").unwrap().unwrap();
    assert!(!b.is_attached() && b.graft == Some(3));
    let before = s.len().unwrap();
    r.transcript("s-1", &all);
    let a = store.attach("dev", "claude", "s-1", None).unwrap();
    let t = store.thread("dev").unwrap();
    let s = t.snapshot().unwrap();
    let b = s.binding("claude", "s-1").unwrap().unwrap();
    assert!(b.is_attached());
    assert_eq!((b.graft, b.transcript.as_deref()), (Some(3), Some(&*p)));
    assert_eq!(a.added.items.start, before);
    let items = s.items().unwrap();
    let parsed = claude::read(&p, "s-1").unwrap();
    assert_eq!(entries(&items[4..]), entries(&parsed));
    assert_eq!(links(&items[5..]), links(&parsed[1..]));
}

#[test]
fn a_session_can_attach_to_another_thread_after_explicit_detach() {
    let r = Repo::new();
    let store = r.store();
    store.create("one").unwrap();
    store.create("two").unwrap();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    store.attach("one", "claude", "s-1", Some(&p)).unwrap();
    let original = store
        .thread("one")
        .unwrap()
        .snapshot()
        .unwrap()
        .items()
        .unwrap();
    assert_eq!(store.detach("claude", "s-1").unwrap(), ["one"]);
    let a = store.attach("two", "claude", "s-1", Some(&p)).unwrap();
    assert_eq!(
        a.added.items,
        0..4,
        "the new thread ingests the whole history"
    );
    assert_eq!(
        store.attached("claude", "s-1").unwrap().unwrap().0.name(),
        "two"
    );
    let t = store.thread("one").unwrap();
    let one = t.snapshot().unwrap();
    assert!(!one.binding("claude", "s-1").unwrap().unwrap().is_attached());
    assert_eq!(one.items().unwrap(), original);
    assert_eq!(
        store
            .thread("two")
            .unwrap()
            .snapshot()
            .unwrap()
            .items()
            .unwrap(),
        original
    );
    let head = r.state("two");
    assert!(
        store
            .attach("two", "claude", "s-1", None)
            .unwrap()
            .added
            .is_empty()
    );
    assert_eq!(
        r.state("two"),
        head,
        "same-thread reattachment with no new data is a no-op"
    );
    let names: Vec<_> = store
        .threads()
        .unwrap()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(names, ["dev", "one", "two"]);
    match sync(&r.homes(), &r.path(), "claude", "s-1", None).unwrap() {
        Synced::Attached { thread, added } => assert!(thread == "two" && added.is_empty()),
        Synced::NotAttached => panic!("attached to two"),
    }
}

#[test]
fn attaching_elsewhere_is_rejected_before_reading_new_sources() {
    let r = Repo::new();
    let store = r.store();
    store.create("one").unwrap();
    store.create("two").unwrap();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    store.attach("one", "claude", "s-1", Some(&path)).unwrap();
    let before = r.state("one");
    let empty = r.state("two");
    let agents = path.with_extension("").join("subagents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(agents.join("agent-.invalid.jsonl"), "").unwrap();

    let error = store.attach("two", "claude", "s-1", None).unwrap_err();
    assert!(error.to_string().contains("Run ai agent detach"));

    assert_eq!(r.state("one"), before);
    assert_eq!(r.state("two"), empty);
    let attached = store.attached("claude", "s-1").unwrap().unwrap();
    assert_eq!(attached.0.name(), "one");
    assert_eq!(attached.1.transcript.as_deref(), Some(path.as_path()));
}

#[test]
fn attaching_elsewhere_is_rejected_without_changing_either_thread() {
    let r = Repo::new();
    let store = r.store();
    store.create("one").unwrap();
    store.create("two").unwrap();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    store.attach("one", "claude", "s-1", Some(&path)).unwrap();
    let before = r.state("one");
    store.attach("two", "claude", "s-2", None).unwrap();
    let destination = r.state("two");

    let error = store.attach("two", "claude", "s-1", None).unwrap_err();
    assert!(error.to_string().contains("Run ai agent detach"));

    assert_eq!(r.state("one"), before);
    assert_eq!(r.state("two"), destination);
    assert_eq!(
        store.attached("claude", "s-1").unwrap().unwrap().0.name(),
        "one"
    );
    assert!(matches!(
        store.sync("claude", "s-1", None).unwrap(),
        Synced::Attached { thread, .. } if thread == "one"
    ));
}

#[test]
fn attaching_records_every_file_of_the_session_and_copies_none() {
    let r = Repo::new();
    let store = r.store();
    let mut original = fs::read(fixture("plain.jsonl")).unwrap();
    original.extend_from_slice(
        b"{\"type\":\"future-event\",\"unknown\":true}\r\n\
          {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"thinking\",\"thinking\":\"hidden reasoning\"}]}}\n\
          {\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"injected context\"}}\n\
          malformed \xff record\n{\"incomplete\":",
    );
    let path = r.dir.path().join("s-1.jsonl");
    let files = [
        ("s-1.jsonl", original),
        (
            "s-1/subagents/agent-a1b2c3d4e5f6a7b8c.jsonl",
            fs::read(fixture("subagent/subagents/agent-a1b2c3d4e5f6a7b8c.jsonl")).unwrap(),
        ),
        (
            "s-1/subagents/agent-a1b2c3d4e5f6a7b8c.meta.json",
            fs::read(fixture(
                "subagent/subagents/agent-a1b2c3d4e5f6a7b8c.meta.json",
            ))
            .unwrap(),
        ),
        (
            "s-1/tool-results/nested/output.bin",
            vec![0, 255, 128, b'\n', b'\r', 0],
        ),
        ("s-1/.session-artifact", b"unrecognised artifact\n".to_vec()),
        ("s-1/empty", Vec::new()),
    ];
    for (name, bytes) in &files {
        let file = r.dir.path().join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
    }
    fs::write(r.dir.path().join("unrelated.json"), "outside session").unwrap();

    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();

    let snapshot = store.thread("dev").unwrap().snapshot().unwrap();
    for (name, bytes) in &files {
        assert_source(&r, "dev", &r.dir.path().join(name), bytes);
    }
    assert_eq!(
        source(&r, "dev", &r.dir.path().join("unrelated.json")),
        None
    );
    assert!(!r.store_dir().join("threads/dev/sources").exists());
    assert_eq!(
        snapshot.items().unwrap(),
        claude::read(&path, "s-1").unwrap()
    );
}

#[test]
fn sync_records_source_changes_without_new_projected_items() {
    let r = Repo::new();
    let store = r.store();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let before = r.state("dev");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"future-event\",\"payload\":true}\nunfinished \xff")
        .unwrap();
    let original = fs::read(&path).unwrap();

    assert!(matches!(
        store.sync("claude", "s-1", None).unwrap(),
        Synced::Attached { added, .. } if added.is_empty()
    ));

    assert_source(&r, "dev", &path, &original);
    assert_ne!(r.state("dev"), before);
    let recorded = r.state("dev");
    store.sync("claude", "s-1", None).unwrap();
    assert_eq!(r.state("dev"), recorded, "unchanged sources write nothing");
}

#[test]
fn a_rewritten_companion_file_is_recorded_again() {
    let r = Repo::new();
    let store = r.store();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let artifact = path.with_extension("").join("tool-results/output.bin");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, b"original\0artifact\xff").unwrap();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let mut changed = fs::read(&path).unwrap();
    changed.extend_from_slice(b"{\"type\":\"future-event\"}\n");
    fs::write(&path, &changed).unwrap();
    fs::write(&artifact, b"changed artifact").unwrap();

    store.sync("claude", "s-1", None).unwrap();

    assert_source(&r, "dev", &path, &changed);
    assert_source(&r, "dev", &artifact, b"changed artifact");
}

/// What a sync told of files that no longer hold what the thread read.
fn unheld(s: Synced) -> Vec<String> {
    match s {
        Synced::Attached { added, .. } => added.unheld,
        Synced::NotAttached => panic!("not attached"),
    }
}

/// The source records of thread `thread`, as they are written.
fn records(r: &Repo, thread: &str) -> Vec<serde_json::Value> {
    text(
        &r.store_dir()
            .join(format!("threads/{thread}/sources.jsonl")),
    )
    .lines()
    .map(|l| serde_json::from_str(l).unwrap())
    .collect()
}

#[test]
fn a_harness_file_gone_or_cut_keeps_its_records_and_a_sync_tells_of_it() {
    const CHECK: &str = "`ai backup threads` tells whether the backup holds them.";
    let r = Repo::new();
    let store = r.store();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let artifact = path.with_extension("").join("tool-results/output.bin");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, b"artifact").unwrap();
    let a = store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    assert_eq!(a.added.unheld, Vec::<String>::new());

    fs::remove_file(&artifact).unwrap();
    let before = r.state("dev");
    let gone = format!(
        "Thread dev: {} is gone; this thread read 8B of it. {CHECK}",
        pretty(&real(&artifact))
    );
    for _ in 0..2 {
        assert_eq!(
            unheld(store.sync("claude", "s-1", None).unwrap()),
            std::slice::from_ref(&gone),
            "a gone file is told on every sync"
        );
    }
    assert_eq!(r.state("dev"), before, "a gone file records nothing");
    assert_source(&r, "dev", &artifact, b"artifact");

    let all = fs::read(&path).unwrap();
    let cut = &all[..all.len() / 2];
    fs::write(&path, cut).unwrap();
    assert_eq!(
        unheld(store.sync("claude", "s-1", None).unwrap()),
        [
            format!(
                "Thread dev: {} no longer begins with the {} this thread read of it. {CHECK}",
                pretty(&real(&path)),
                human(all.len() as u64)
            ),
            gone.clone(),
        ]
    );
    assert_source(&r, "dev", &path, cut);
    assert_eq!(
        unheld(store.sync("claude", "s-1", None).unwrap()),
        [gone],
        "a changed file is told once, on the sync that finds it changed"
    );
    let snap = store.thread("dev").unwrap().snapshot().unwrap();
    let of_path: Vec<u64> = snap
        .versions()
        .unwrap()
        .iter()
        .filter(|v| v.read.path == real(&path))
        .map(|v| v.read.len)
        .collect();
    assert_eq!(
        of_path,
        [all.len() as u64, cut.len() as u64],
        "a cut file begins a version, and the one before stays recorded"
    );
}

#[test]
fn a_subagent_meta_file_rewritten_begins_a_version_untold() {
    let r = Repo::new();
    let dir = r.dir.path().join("s-1/subagents");
    fs::create_dir_all(&dir).unwrap();
    for f in [
        "agent-a1b2c3d4e5f6a7b8c.jsonl",
        "agent-a1b2c3d4e5f6a7b8c.meta.json",
    ] {
        fs::copy(fixture("subagent/subagents").join(f), dir.join(f)).unwrap();
    }
    let meta = dir.join("agent-a1b2c3d4e5f6a7b8c.meta.json");
    let first = fs::read(&meta).unwrap();
    let p = r.transcript("s-1", &lines("subagent.jsonl"));
    let store = r.store();
    store.attach("dev", "claude", "s-1", Some(&p)).unwrap();

    let rewritten = br#"{"agentType":"fork","description":"rewritten"}"#;
    assert!(!rewritten.starts_with(&first));
    fs::write(&meta, rewritten).unwrap();
    assert_eq!(
        unheld(store.sync("claude", "s-1", None).unwrap()),
        Vec::<String>::new(),
        "Claude Code rewrites a subagent's meta file by design"
    );
    assert_source(&r, "dev", &meta, rewritten);
    let snap = store.thread("dev").unwrap().snapshot().unwrap();
    let of_meta: Vec<u64> = snap
        .versions()
        .unwrap()
        .iter()
        .filter(|v| v.read.path == real(&meta))
        .map(|v| v.read.len)
        .collect();
    assert_eq!(of_meta, [first.len() as u64, rewritten.len() as u64]);
}

#[test]
fn a_harness_file_is_recorded_where_it_really_sits() {
    let r = Repo::new();
    let elsewhere = r.dir.path().join("elsewhere");
    fs::create_dir_all(elsewhere.join("-x")).unwrap();
    let link = r.dir.path().join("linked");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    let path = link.join("-x/s-1.jsonl");
    fs::write(&path, lines("plain.jsonl").concat()).unwrap();
    let store = r.store();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();

    let sits = fs::canonicalize(&elsewhere).unwrap().join("-x/s-1.jsonl");
    let snap = store.thread("dev").unwrap().snapshot().unwrap();
    assert_eq!(
        snap.bindings().unwrap()[0].transcript.as_deref(),
        Some(sits.as_path())
    );
    let versions = snap.versions().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].read.path, sits);
    assert_eq!(versions[0].read.host, host().unwrap());

    fs::OpenOptions::new()
        .append(true)
        .open(&sits)
        .unwrap()
        .write_all(b"{\"type\":\"future-event\"}\n")
        .unwrap();
    store.sync("claude", "s-1", None).unwrap();
    assert_source(&r, "dev", &sits, &fs::read(&sits).unwrap());
}

#[test]
fn a_file_that_grows_is_recorded_as_going_on_from_what_was_read() {
    let r = Repo::new();
    let store = r.store();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let artifact = path.with_extension("").join("tool-results/log.txt");
    let empty = path.with_extension("").join("empty");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, b"one\n").unwrap();
    fs::write(&empty, b"").unwrap();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    fs::write(&artifact, b"one\ntwo\n").unwrap();
    fs::write(&empty, b"no longer\n").unwrap();
    store.sync("claude", "s-1", None).unwrap();

    let of = |p: &Path| -> Vec<serde_json::Value> {
        let at = real(p);
        let mut rows = records(&r, "dev");
        rows.retain(|v| v["path"].as_str() == at.to_str());
        rows
    };
    let mut rows = of(&artifact);
    assert_eq!(rows.len(), 2);
    for row in &mut rows {
        let at = row.as_object_mut().unwrap().remove("at").unwrap();
        at.as_str().unwrap().parse::<jiff::Timestamp>().unwrap();
    }
    assert_eq!(
        rows[1],
        serde_json::json!({
            "harness": "claude",
            "session": "s-1",
            "host": host().unwrap(),
            "path": real(&artifact),
            "from": 4,
            "len": 8,
            "sha256": sha256(b"one\ntwo\n"),
        }),
        "the second record goes on from the first's length"
    );
    assert_eq!(
        (rows[0]["from"].as_u64(), rows[0]["len"].as_u64()),
        (Some(0), Some(4))
    );
    assert_source(&r, "dev", &artifact, b"one\ntwo\n");

    let snap = store.thread("dev").unwrap().snapshot().unwrap();
    let versions = snap.versions().unwrap();
    assert_eq!(
        versions
            .iter()
            .filter(|v| v.read.path == real(&empty))
            .count(),
        1,
        "a version of no bytes gives way to the one that begins next"
    );
    assert_source(&r, "dev", &empty, b"no longer\n");
}

#[test]
fn a_source_record_that_goes_on_from_nothing_is_an_error_naming_it() {
    let r = Repo::new();
    let store = r.store();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let file = r.store_dir().join("threads/dev/sources.jsonl");
    let mut row = records(&r, "dev").remove(0);
    row["from"] = 7.into();
    row["path"] = "/elsewhere/s-9.jsonl".into();
    fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(format!("{row}\n").as_bytes())
        .unwrap();
    let e = store.thread("dev").unwrap().snapshot().unwrap().versions();
    assert_eq!(
        e.unwrap_err().to_string(),
        "Thread dev: source record 2 goes on from byte 7 of /elsewhere/s-9.jsonl, which no record \
         before it began."
    );
}

#[test]
fn subagents_are_kept_beside_the_main_tree() {
    let r = Repo::new();
    let dir = r.dir.path().join("s-1/subagents");
    fs::create_dir_all(&dir).unwrap();
    for f in [
        "agent-a1b2c3d4e5f6a7b8c.jsonl",
        "agent-a1b2c3d4e5f6a7b8c.meta.json",
    ] {
        fs::copy(fixture("subagent/subagents").join(f), dir.join(f)).unwrap();
    }
    let p = r.transcript("s-1", &lines("subagent.jsonl"));
    let store = r.store();
    let a = store.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let read = read("claude", "s-1", Some(&p)).unwrap();
    assert_eq!(a.added.items, 0..read.items.len() as u64);
    assert_eq!(a.added.agent_items, 6);
    let thread = store.thread("dev").unwrap();
    let s = thread.snapshot().unwrap();
    assert_eq!(
        s.items().unwrap(),
        read.items,
        "the main tree holds no subagent item"
    );
    let agents = s.agents().unwrap();
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(
        (
            agent.agent.as_str(),
            agent.agent_type.as_str(),
            agent.description.as_str()
        ),
        (
            "a1b2c3d4e5f6a7b8c",
            "general-purpose",
            "Survey thread storage"
        )
    );
    let call = s.item(agent.call.unwrap()).unwrap();
    assert!(matches!(call.kind, Kind::ToolCall { ref id, .. } if id == "toolu_50"));
    let items = s.agent_items("claude", &agent.agent).unwrap();
    assert_eq!(items, read.agents[0].items);
    let head = r.state("dev");
    let again = sync(&r.homes(), &r.path(), "claude", "s-1", None).unwrap();
    assert!(matches!(again, Synced::Attached { added, .. } if added.is_empty()));
    assert_eq!(r.state("dev"), head);
}

#[test]
fn a_missing_main_transcript_is_told_gone_and_keeps_subagent_linkage() {
    let r = Repo::new();
    let path = r.transcript("s-1", &lines("subagent.jsonl"));
    let agents = path.with_extension("").join("subagents");
    fs::create_dir_all(&agents).unwrap();
    for name in [
        "agent-a1b2c3d4e5f6a7b8c.jsonl",
        "agent-a1b2c3d4e5f6a7b8c.meta.json",
    ] {
        fs::copy(fixture("subagent/subagents").join(name), agents.join(name)).unwrap();
    }
    let store = r.store();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let thread = store.thread("dev").unwrap();
    let before = thread.snapshot().unwrap().agents().unwrap();
    assert!(before[0].call.is_some());
    fs::remove_file(&path).unwrap();

    let told = unheld(store.sync("claude", "s-1", None).unwrap());

    assert!(
        told.len() == 1 && told[0].contains(&format!("{} is gone", pretty(&real(&path)))),
        "{told:?}"
    );
    assert_eq!(thread.snapshot().unwrap().agents().unwrap(), before);
}

#[test]
fn lines_are_built_once() {
    let r = Repo::new();
    let store = r.store();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    store.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let t = store.thread("dev").unwrap();
    let who = Who {
        agent: "claude-code".into(),
        model: "claude-haiku-5-5".into(),
        session: "c-1".into(),
    };
    let key = Key { level: 1, last: 1 };
    let first = t
        .put_line(key, LineKind::Turn, &who, "user: hi; ai: hello")
        .unwrap();
    let again = t
        .put_line(key, LineKind::Turn, &Who::default(), "other")
        .unwrap();
    assert_eq!(again, first, "a built line never changes");
    t.put_line(Key { level: 1, last: 3 }, LineKind::Turn, &who, "second")
        .unwrap();
    t.put_line(Key { level: 0, last: 3 }, LineKind::Chunk, &who, "chunk")
        .unwrap();
    let s = t.snapshot().unwrap();
    assert_eq!(s.line(key).unwrap(), Some(first.clone()));
    assert_eq!(s.line(Key { level: 2, last: 3 }).unwrap(), None);
    let level: Vec<_> = s
        .lines(1)
        .unwrap()
        .into_iter()
        .map(|l| (l.0, l.1.text))
        .collect();
    assert_eq!(
        level,
        [(1, "user: hi; ai: hello".into()), (3, "second".into())]
    );
    assert_eq!(first.who, who);
    let err = t
        .put_line(Key { level: 0, last: 4 }, LineKind::Chunk, &who, "x")
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Line 0@4 is beyond the thread: it holds 4 items."
    );
}

#[test]
fn the_compactor_lock_is_per_thread() {
    let r = Repo::new();
    let store = r.store();
    let (a, b) = (store.create("a").unwrap(), store.create("b").unwrap());
    let held = a.try_lock().unwrap();
    assert!(held.is_some());
    assert!(r.store().thread("a").unwrap().try_lock().unwrap().is_none());
    assert!(b.try_lock().unwrap().is_some());
    drop(held);
    assert!(a.try_lock().unwrap().is_some());
    assert!(r.local_dir().join("threads/a/compact.lock").is_file());
}

#[test]
fn a_commit_takes_the_projects_folder_and_leaves_the_rest_of_the_repository() {
    let r = Repo::new();
    let store = r.store();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    store.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let t = store.create("other").unwrap();
    assert!(t.try_lock().unwrap().is_some());
    let ai = r.homes().repo;
    fs::write(ai.join("notes.md"), "staged\n").unwrap();
    git(&ai, &["add", "notes.md"]);
    fs::write(ai.join("draft.md"), "untracked\n").unwrap();
    store.commit("ai agent attach dev").unwrap();

    assert_eq!(git(&ai, &["log", "--format=%s"]), "ai agent attach dev\n");
    let files = git(&ai, &["ls-tree", "-r", "--name-only", "HEAD"]);
    for f in [
        "projects/repo/id",
        "projects/repo/ai.toml",
        "projects/repo/agents/format",
        "projects/repo/agents/threads/dev/attachments.jsonl",
        "projects/repo/agents/threads/dev/items.jsonl",
        "projects/repo/agents/threads/dev/sessions.jsonl",
        "projects/repo/agents/threads/dev/sources.jsonl",
    ] {
        assert!(files.lines().any(|l| l == f), "{f} in {files}");
    }
    assert!(
        files.lines().all(|l| l.starts_with("projects/repo/")),
        "only the project's folder is committed: {files}"
    );
    assert!(
        !files.contains(".lock") && !files.contains(".log"),
        "{files}"
    );
    assert_eq!(
        git(&ai, &["status", "--porcelain"]),
        "A  notes.md\n?? draft.md\n",
        "what the user staged stays staged, and the project's folder is clean"
    );

    let mut root: Vec<String> = fs::read_dir(r.store_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    root.sort();
    assert_eq!(
        root,
        ["format", "threads"],
        "the store's root holds its format; locks are this machine's"
    );
    assert!(r.local_dir().join("threads/other/compact.lock").is_file());

    store.commit("again").unwrap();
    assert_eq!(git(&ai, &["log", "--format=%s"]), "ai agent attach dev\n");
    assert_eq!(
        store.create("dev").err().unwrap().to_string(),
        "Agent dev already exists in this project."
    );
}

#[test]
fn a_session_attached_to_two_threads_is_an_error_until_it_is_detached() {
    let r = Repo::new();
    let store = r.store();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    store.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    store.create("other").unwrap();
    let events = r.store_dir().join("threads/dev/attachments.jsonl");
    fs::copy(
        &events,
        r.store_dir().join("threads/other/attachments.jsonl"),
    )
    .unwrap();
    let e = sync(&r.homes(), &r.path(), "claude", "s-1", None).unwrap_err();
    assert_eq!(
        e.to_string(),
        "Session claude s-1 is attached to dev and other. Detach it, which detaches it from \
         each (ai agent detach --harness claude --session s-1), then attach it to one."
    );
    assert_eq!(store.detach("claude", "s-1").unwrap(), ["dev", "other"]);
    assert_eq!(
        sync(&r.homes(), &r.path(), "claude", "s-1", None).unwrap(),
        Synced::NotAttached
    );
}

#[test]
fn worktrees_share_the_threads() {
    let r = Repo::new();
    git(&r.path(), &["commit", "-q", "--allow-empty", "-m", "root"]);
    let wt = r.dir.path().join("wt");
    git(&r.path(), &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    let sub = wt.join("deep");
    fs::create_dir_all(&sub).unwrap();
    let store = Store::find_in(&r.homes(), &sub).unwrap().unwrap();
    store.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let names: Vec<String> = r
        .store()
        .threads()
        .unwrap()
        .iter()
        .map(|t| t.name().into())
        .collect();
    assert_eq!(names, ["dev"]);
    assert!(r.store_dir().join("threads/dev").is_dir());
    assert!(!wt.join(".ai").exists());
    assert!(store.thread("dev").unwrap().try_lock().unwrap().is_some());
    assert!(r.local_dir().join("threads/dev/compact.lock").is_file());
    let main = r.store();
    assert_eq!(
        main.thread("dev")
            .unwrap()
            .snapshot()
            .unwrap()
            .len()
            .unwrap(),
        4
    );
    assert!(matches!(
        sync(&r.homes(), &r.path(), "claude", "s-1", None).unwrap(),
        Synced::Attached { .. }
    ));
}

#[test]
fn a_newer_store_is_refused() {
    let r = Repo::new();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    r.store().attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let before = r.state("dev");
    fs::write(r.store_dir().join("format"), "7\n").unwrap();
    let want = "This agent store is format 7; this ai reads up to 6. Upgrade ai.";
    let found = Store::find_in(&r.homes(), &r.path()).map(|_| ());
    assert_eq!(found.unwrap_err().to_string(), want);
    let s = sync(&r.homes(), &r.path(), "claude", "s-1", None).map(|_| ());
    assert_eq!(s.unwrap_err().to_string(), want);
    assert_eq!(r.state("dev"), before);
}

#[test]
fn rows_keep_source_ids_exact() {
    let r = Repo::new();
    let path = r.transcript("s-1", &relabel(&lines("plain.jsonl"), "with space "));
    let store = r.store();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let snapshot = store.thread("dev").unwrap().snapshot().unwrap();

    assert_eq!(
        snapshot.items().unwrap(),
        claude::read(&path, "s-1").unwrap()
    );
    let rows = text(&r.store_dir().join("threads/dev/items.jsonl"));
    let first: serde_json::Value = serde_json::from_str(rows.lines().next().unwrap()).unwrap();
    assert_eq!(first["entry"], "with space u1");
    assert_eq!(first["kind"], "user");
}

/// Appends `line` to the file `name` of thread `dev`.
fn corrupt(r: &Repo, name: &str, line: serde_json::Value) {
    let p = r.store_dir().join("threads/dev").join(name);
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .unwrap();
    writeln!(f, "{line}").unwrap();
}

#[test]
fn corrupt_records_are_errors_naming_thread_and_record() {
    let track = |tip: u64, graft: Option<u64>| serde_json::json!({"harness": "claude", "session": "s-1", "transcript": null, "config": null, "graft": graft, "tip": tip});
    let cases = [
        ("sessions.jsonl", track(9999, None), "tip 9999"),
        ("sessions.jsonl", track(0, Some(9999)), "graft 9999"),
        (
            "sessions.jsonl",
            serde_json::json!({"harness": "claude", "session": "s-1", "graft": null, "tip": 0}),
            "session record 2 is corrupt",
        ),
        (
            "items.jsonl",
            serde_json::json!({"seq": 4, "parent": 4, "harness": "claude", "session": "s-1", "entry": "x", "ts": "2026-10-14T10:00:00Z", "kind": "text", "content": "t"}),
            "item 4 is corrupt",
        ),
        (
            "agents.jsonl",
            serde_json::json!({"harness": "claude", "agent": "a1", "agent_type": "t", "description": "d", "call": 9999}),
            "call 9999",
        ),
    ];
    for (file, line, want) in cases {
        let r = Repo::new();
        let p = r.transcript("s-1", &lines("plain.jsonl"));
        let store = r.store();
        store.attach("dev", "claude", "s-1", Some(&p)).unwrap();
        corrupt(&r, file, line);
        let s = store.thread("dev").unwrap().snapshot().unwrap();
        let e = match file {
            "agents.jsonl" => s.agents().map(|_| ()).unwrap_err(),
            "items.jsonl" => s.tree().map(|_| ()).unwrap_err(),
            _ => s.binding("claude", "s-1").map(|_| ()).unwrap_err(),
        };
        let e = ai_tools::error::line(&e);
        assert!(e.contains("Thread dev") && e.contains(want), "{file}: {e}");
        if file != "agents.jsonl" {
            let e = ai_tools::error::line(&store.list().unwrap_err());
            assert!(e.contains("Thread dev") && e.contains(want), "{file}: {e}");
        }
    }
}

#[test]
fn a_corrupt_thread_leaves_other_sessions_working() {
    let r = Repo::new();
    let store = r.store();
    let pa = r.transcript("s-a", &lines("plain.jsonl"));
    let pb = r.transcript("s-b", &relabel(&lines("plain.jsonl"), "b-"));
    store.attach("dev", "claude", "s-a", Some(&pa)).unwrap();
    store.create("other").unwrap();
    store.attach("other", "claude", "s-b", Some(&pb)).unwrap();
    corrupt(&r, "items.jsonl", serde_json::json!({"seq": "broken"}));

    assert!(store.sync("claude", "s-a", None).is_err());
    assert!(matches!(
        store.sync("claude", "s-b", None).unwrap(),
        Synced::Attached { thread, .. } if thread == "other"
    ));
    assert_eq!(store.detach("claude", "s-b").unwrap(), ["other"]);
    let e = store.detach("claude", "s-a").unwrap_err().to_string();
    assert_eq!(e, "Detached from dev without its newest entries");
    store.attach("other", "claude", "s-a", Some(&pa)).unwrap();
}

#[test]
fn names_and_ids_are_checked() {
    let r = Repo::new();
    let store = r.store();
    for bad in ["", "-x", "a/b", "a.b", "a b"] {
        assert!(store.thread(bad).is_err(), "{bad}");
    }
    assert!(store.attach("dev", "claude", "a/b", None).is_err());
    assert!(store.attach("dev", "", "s-1", None).is_err());
    assert!(store.attached("claude", "s-1").unwrap().is_none());
}

const CHILD: &str = "AI_AGENT_TEST_SYNC";

/// Run by [`two_processes_syncing_one_thread_lose_nothing`] as a child process: syncs the
/// session `$AI_AGENT_TEST_SYNC` names (`<repo> <session>`); nothing otherwise.
#[test]
fn sync_child() {
    let Ok(arg) = std::env::var(CHILD) else {
        return;
    };
    let (repo, session) = arg.split_once(' ').unwrap();
    let s = sync(
        &homes_of(Path::new(repo)),
        Path::new(repo),
        "claude",
        session,
        None,
    )
    .unwrap();
    assert!(matches!(s, Synced::Attached { .. }));
}

#[test]
fn two_processes_syncing_one_thread_lose_nothing() {
    let r = Repo::new();
    let a = lines("interject.jsonl");
    let b = relabel(&lines("tools.jsonl"), "b-");
    let pa = r.transcript("s-a", &a[..1]);
    let pb = r.transcript("s-b", &b[..1]);
    r.store().attach("dev", "claude", "s-a", Some(&pa)).unwrap();
    r.store().attach("dev", "claude", "s-b", Some(&pb)).unwrap();
    let steps = a.len().max(b.len());
    let me = std::env::current_exe().unwrap();
    for k in 1..=steps {
        r.transcript("s-a", &a[..k.min(a.len())]);
        r.transcript("s-b", &b[..k.min(b.len())]);
        let kids: Vec<_> = ["s-a", "s-b", "s-a", "s-b"]
            .into_iter()
            .map(|s| {
                Command::new(&me)
                    .args(["--exact", "sync_child", "--test-threads", "1"])
                    .env(CHILD, format!("{} {s}", r.path().display()))
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for kid in kids {
            let o = kid.wait_with_output().unwrap();
            assert!(
                o.status.success(),
                "a sync failed at step {k}:\n{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
        }
    }
    let t = r.store().thread("dev").unwrap();
    let s = t.snapshot().unwrap();
    let items = s.items().unwrap();
    let (ia, ib) = (
        claude::read(&pa, "s-a").unwrap(),
        claude::read(&pb, "s-b").unwrap(),
    );
    let of = |session: &str| -> Vec<Item> {
        items
            .iter()
            .filter(|i| i.id.session == session)
            .cloned()
            .collect()
    };
    assert_eq!(entries(&of("s-a")), entries(&ia));
    assert_eq!(entries(&of("s-b")), entries(&ib));
    assert_eq!(items.len(), ia.len() + ib.len(), "each entry once");
    assert_eq!(links(&of("s-a"))[1..], links(&ia)[1..]);
    assert_eq!(links(&of("s-b"))[1..], links(&ib)[1..]);
    let records = text(&r.store_dir().join("threads/dev/sessions.jsonl"));
    assert!(
        records.lines().count() <= 2 + 2 * steps,
        "a sync with nothing new writes nothing"
    );
}
