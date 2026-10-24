use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use ai_tools::{
    agent::{Item, Key, Kind, LineKind, Store, Synced, Tree, claude, read, sync, upgrade},
    memory::Who,
    project::Project,
    segments,
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
        Project::init(&repo).unwrap();
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
        Store::find(&self.path()).unwrap().unwrap()
    }

    /// Writes the transcript of `session`.
    fn transcript(&self, session: &str, lines: &[String]) -> PathBuf {
        let p = self.dir.path().join(format!("{session}.jsonl"));
        fs::write(&p, lines.concat()).unwrap();
        p
    }

    /// The files of thread `name`, by path within it; `None` before it exists.
    fn state(&self, name: &str) -> Option<BTreeMap<PathBuf, Vec<u8>>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
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

    /// The agent store of the project, in its checkout.
    fn store_dir(&self) -> PathBuf {
        self.path().join(".ai/local/agents")
    }
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

/// The segmented file `p` as text.
fn text(p: &Path) -> String {
    String::from_utf8(segments::read_all(p).unwrap()).unwrap()
}

fn assert_source(r: &Repo, thread: &str, path: &str, expected: &[u8]) {
    let p = r.store_dir().join("threads").join(thread).join(path);
    assert!(segments::is_segmented(&p), "missing original file: {path}");
    let bytes = segments::read_all(&p).unwrap();
    assert!(
        bytes == expected,
        "source bytes differ for {path}: expected {} bytes, found {}",
        expected.len(),
        bytes.len()
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
    let more = added(sync(&r.path(), "claude", "s-1", None).unwrap());
    assert_eq!(more.start, first.end);
    let head = r.state("dev");
    let none = added(sync(&r.path(), "claude", "s-1", Some(&path)).unwrap());
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
        "4\n"
    );
    let events = text(&r.store_dir().join("sessions.jsonl"));
    assert_eq!(events.lines().count(), 1, "one attachment: {events}");
    let rows = text(&r.store_dir().join("threads/dev/items.jsonl"));
    assert_eq!(rows.lines().count() as u64, more.end);
}

#[test]
fn an_unattached_session_is_not_attached() {
    let r = Repo::new();
    let outside = TempDir::new().unwrap();
    let s = sync(outside.path(), "claude", "s-1", None).unwrap();
    assert_eq!(s, Synced::NotAttached, "outside a repository");
    assert_eq!(
        sync(&r.path(), "claude", "s-1", None).unwrap(),
        Synced::NotAttached
    );
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let store = r.store();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    assert_eq!(
        sync(&r.path(), "claude", "s-2", None).unwrap(),
        Synced::NotAttached
    );
    assert_eq!(store.detach("claude", "s-1").unwrap(), Some("dev".into()));
    assert_eq!(store.detach("claude", "s-1").unwrap(), None);
    let head = r.state("dev");
    assert_eq!(
        sync(&r.path(), "claude", "s-1", Some(&path)).unwrap(),
        Synced::NotAttached
    );
    assert_eq!(r.state("dev"), head);
    assert!(sync(&r.path(), "claude", "../s-1", None).is_err());
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
    assert_eq!(added(sync(&r.path(), "claude", "s-1", None).unwrap()), 0..4);
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
    assert_eq!(store.detach("claude", "s-1").unwrap(), Some("one".into()));
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
    match sync(&r.path(), "claude", "s-1", None).unwrap() {
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
fn attaching_archives_complete_original_session_files() {
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
        assert_source(&r, "dev", &format!("sources/claude/s-1/{name}"), bytes);
    }
    let unrelated = r
        .store_dir()
        .join("threads/dev/sources/claude/s-1/unrelated.json");
    assert!(!unrelated.exists());
    assert_eq!(
        snapshot.items().unwrap(),
        claude::read(&path, "s-1").unwrap()
    );
}

#[test]
fn sync_archives_source_changes_without_new_projected_items() {
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

    assert_source(&r, "dev", "sources/claude/s-1/s-1.jsonl", &original);
    assert_ne!(r.state("dev"), before);
    let archived = r.state("dev");
    store.sync("claude", "s-1", None).unwrap();
    assert_eq!(r.state("dev"), archived, "unchanged sources write nothing");
}

#[test]
fn original_files_survive_source_changes_and_disappearance() {
    let r = Repo::new();
    let store = r.store();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let original = fs::read(&path).unwrap();
    let artifact = path.with_extension("").join("tool-results/output.bin");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, b"original\0artifact\xff").unwrap();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let mut changed = original.clone();
    changed.extend_from_slice(b"{\"type\":\"future-event\"}\n");
    fs::write(&path, &changed).unwrap();
    fs::write(&artifact, b"changed artifact").unwrap();
    store.sync("claude", "s-1", None).unwrap();
    fs::remove_file(&path).unwrap();
    fs::remove_file(&artifact).unwrap();

    store.sync("claude", "s-1", None).unwrap();

    assert_source(&r, "dev", "sources/claude/s-1/s-1.jsonl", &changed);
    assert_source(
        &r,
        "dev",
        "sources/claude/s-1/s-1/tool-results/output.bin",
        b"changed artifact",
    );
}

#[test]
fn a_source_path_changing_kind_replaces_the_archived_entry() {
    let r = Repo::new();
    let store = r.store();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let companion = path.with_extension("");
    fs::create_dir_all(companion.join("x")).unwrap();
    fs::write(companion.join("x/inner"), b"inner").unwrap();
    store.attach("dev", "claude", "s-1", Some(&path)).unwrap();

    fs::remove_dir_all(companion.join("x")).unwrap();
    fs::write(companion.join("x"), b"now a file").unwrap();
    store.sync("claude", "s-1", None).unwrap();
    assert_source(&r, "dev", "sources/claude/s-1/s-1/x", b"now a file");

    fs::remove_file(companion.join("x")).unwrap();
    fs::create_dir_all(companion.join("x")).unwrap();
    fs::write(companion.join("x/inner"), b"a dir again").unwrap();
    store.sync("claude", "s-1", None).unwrap();
    assert_source(&r, "dev", "sources/claude/s-1/s-1/x/inner", b"a dir again");
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
    let again = sync(&r.path(), "claude", "s-1", None).unwrap();
    assert!(matches!(again, Synced::Attached { added, .. } if added.is_empty()));
    assert_eq!(r.state("dev"), head);
}

#[test]
fn missing_main_transcript_preserves_subagent_linkage() {
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

    store.sync("claude", "s-1", None).unwrap();

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
    let (a, b) = (store.thread("a").unwrap(), store.thread("b").unwrap());
    let held = a.try_lock().unwrap();
    assert!(held.is_some());
    assert!(r.store().thread("a").unwrap().try_lock().unwrap().is_none());
    assert!(b.try_lock().unwrap().is_some());
    drop(held);
    assert!(a.try_lock().unwrap().is_some());
    assert!(r.store_dir().join("a.lock").is_file());
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
    let store = Store::find(&sub).unwrap().unwrap();
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
    assert!(r.store_dir().join("dev.lock").is_file());
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
        sync(&r.path(), "claude", "s-1", None).unwrap(),
        Synced::Attached { .. }
    ));
}

#[test]
fn a_newer_store_is_refused() {
    let r = Repo::new();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    r.store().attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let before = r.state("dev");
    fs::write(r.store_dir().join("format"), "5\n").unwrap();
    let want = "This agent store is format 5; this ai reads up to 4. Upgrade ai.";
    let found = Store::find(&r.path()).map(|_| ());
    assert_eq!(found.unwrap_err().to_string(), want);
    let s = sync(&r.path(), "claude", "s-1", None).map(|_| ());
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
    segments::extend(&p, format!("{line}\n").as_bytes()).unwrap();
}

#[test]
fn corrupt_records_are_errors_naming_thread_and_record() {
    let track = |tip: u64, graft: Option<u64>| serde_json::json!({"harness": "claude", "session": "s-1", "transcript": null, "graft": graft, "tip": tip});
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
        let e = format!("{e:#}");
        assert!(e.contains("Thread dev") && e.contains(want), "{file}: {e}");
        if file != "agents.jsonl" {
            let e = format!("{:#}", store.list().unwrap_err());
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
    assert_eq!(store.detach("claude", "s-b").unwrap(), Some("other".into()));
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
    let s = sync(Path::new(repo), "claude", session, None).unwrap();
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

/// A thread with a session, a subagent and a line, in format 4: the thread's files and the
/// store's `sessions.jsonl`.
fn full_thread(r: &Repo) -> (Option<BTreeMap<PathBuf, Vec<u8>>>, String) {
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
    store.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let t = store.thread("dev").unwrap();
    t.put_line(
        Key { level: 1, last: 0 },
        LineKind::Turn,
        &Who::default(),
        "a line",
    )
    .unwrap();
    (r.state("dev"), text(&r.store_dir().join("sessions.jsonl")))
}

/// Makes the store format 3, as ai 0.14 wrote it: each segmented file joined into one file.
fn downgrade(store: &Path) {
    fn join(p: &Path) {
        let b = segments::read_all(p).unwrap();
        fs::remove_dir_all(p).unwrap();
        fs::write(p, b).unwrap();
    }
    fn walk(dir: &Path) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            match (p.is_dir(), segments::is_segmented(&p)) {
                (true, true) => join(&p),
                (true, false) => walk(&p),
                _ => {}
            }
        }
    }
    walk(&store.join("threads"));
    join(&store.join("sessions.jsonl"));
    fs::write(store.join("format"), "3\n").unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let (p, to) = (e.as_ref().unwrap().path(), to.join(e.unwrap().file_name()));
        match p.is_dir() {
            true => copy_dir(&p, &to),
            false => drop(fs::copy(&p, &to).unwrap()),
        }
    }
}

const CONVERT: &str = "This project's agents are stored in format 3. Run an ai agent command on a terminal to convert them to format 4.";

#[test]
fn a_format_3_store_converts_once_asked() {
    let r = Repo::new();
    let (thread, events) = full_thread(&r);
    downgrade(&r.store_dir());
    let old = r.state("dev");
    assert_ne!(old, thread);
    assert_eq!(
        Store::find(&r.path()).map(|_| ()).unwrap_err().to_string(),
        CONVERT
    );
    let s = sync(&r.path(), "claude", "s-1", None).map(|_| ());
    assert_eq!(s.unwrap_err().to_string(), CONVERT);

    let no = upgrade::offer(&r.path(), |_| Ok(false)).unwrap_err();
    assert_eq!(
        no.to_string(),
        "Conversion cancelled; this project's agents are unchanged."
    );
    assert_eq!(r.state("dev"), old);

    let mut asked = String::new();
    upgrade::offer(&r.path(), |q| {
        asked = q.into();
        Ok(true)
    })
    .unwrap();
    assert!(asked.ends_with("to format 4, splitting their growing files into 1 MiB segments?"));
    assert_eq!(r.state("dev"), thread, "every byte, segmented as before");
    assert_eq!(text(&r.store_dir().join("sessions.jsonl")), events);
    assert_eq!(
        fs::read_to_string(r.store_dir().join("format")).unwrap(),
        "4\n"
    );
    let ai = r.path().join(".ai/local");
    assert_eq!(
        git(&ai, &["log", "--format=%s"]),
        "ai agent: convert to format 4\n"
    );
    assert_eq!(git(&ai, &["status", "--porcelain"]), "");
    upgrade::offer(&r.path(), |_| panic!("asked again")).unwrap();
    assert!(matches!(
        sync(&r.path(), "claude", "s-1", None).unwrap(),
        Synced::Attached { added, .. } if added.is_empty()
    ));
}

#[test]
fn a_conversion_cut_short_is_finished() {
    let r = Repo::new();
    let (thread, _) = full_thread(&r);
    let store = r.store_dir();
    let converted = r.dir.path().join("converted");
    copy_dir(&store, &converted);
    downgrade(&store);
    let git = Project::find(&r.path())
        .unwrap()
        .unwrap()
        .repo()
        .unwrap()
        .join(".git");
    let (new, old) = (git.join("ai-agents-v4"), git.join("ai-agents-v3"));

    fs::create_dir_all(new.join("threads")).unwrap();
    upgrade::offer(&r.path(), |_| Ok(true)).unwrap();
    assert_eq!(r.state("dev"), thread, "a build cut short starts again");
    assert!(!new.exists());

    fs::remove_dir_all(&store).unwrap();
    copy_dir(&converted, &new);
    copy_dir(&converted, &store);
    downgrade(&store);
    fs::rename(&store, &old).unwrap();
    assert_eq!(
        Store::find(&r.path()).map(|_| ()).unwrap_err().to_string(),
        CONVERT
    );
    upgrade::offer(&r.path(), |_| Ok(true)).unwrap();
    assert_eq!(r.state("dev"), thread, "a swap cut short is finished");
    assert!(!new.exists() && !old.exists());
}

#[test]
fn a_store_that_is_not_format_3_is_not_converted() {
    let r = Repo::new();
    full_thread(&r);
    downgrade(&r.store_dir());
    fs::write(r.store_dir().join("threads/dev/stray"), "?").unwrap();
    let e = upgrade::offer(&r.path(), |_| Ok(true))
        .unwrap_err()
        .to_string();
    assert!(
        e.starts_with("Cannot convert the agent store: ")
            && e.ends_with("stray is not part of a format-3 store."),
        "{e}"
    );
    assert_eq!(
        fs::read_to_string(r.store_dir().join("format")).unwrap(),
        "3\n"
    );
}
