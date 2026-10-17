use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use ai_tools::{
    chat::{Chat, Item, Key, Kind, LineKind, Snapshot, Synced, Tree, claude, read, sync},
    memory::Who,
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

/// A git repository in a temporary directory, with transcripts beside it.
struct Repo {
    dir: TempDir,
}

impl Repo {
    fn new() -> Repo {
        let dir = TempDir::new().unwrap();
        gix::init(dir.path().join("repo")).unwrap();
        Repo { dir }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn chat(&self) -> Chat {
        Chat::find(&self.path()).unwrap().unwrap()
    }

    /// Writes the transcript of `session`.
    fn transcript(&self, session: &str, lines: &[String]) -> PathBuf {
        let p = self.dir.path().join(format!("{session}.jsonl"));
        fs::write(&p, lines.concat()).unwrap();
        p
    }

    fn head(&self, name: &str) -> Option<String> {
        let repo = gix::open(self.path()).unwrap();
        let r = repo
            .try_find_reference(format!("refs/ai/chat/{name}").as_str())
            .unwrap()?;
        Some(r.id().to_string())
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

fn assert_blob(snapshot: &Snapshot, path: &str, expected: &[u8]) {
    let stored = snapshot.as_ref().entry(path).unwrap();
    assert!(stored.is_some(), "missing original file: {path}");
    let bytes = snapshot.as_ref().read(path).unwrap();
    assert!(
        &*bytes == expected,
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
    let chat = r.chat();
    let a = chat.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    assert!(a.created);
    let first = a.added.items.clone();
    assert!(!first.is_empty() && first.start == 0);

    r.transcript("s-1", &all);
    let more = added(sync(&r.path(), "claude", "s-1", None).unwrap());
    assert_eq!(more.start, first.end);
    let head = r.head("dev");
    let none = added(sync(&r.path(), "claude", "s-1", Some(&path)).unwrap());
    assert!(none.is_empty());
    assert_eq!(r.head("dev"), head, "no commit when nothing is new");

    let want = claude::read(&path, "s-1").unwrap();
    let thread = chat.thread("dev").unwrap();
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
    let log = git(&r.path(), &["log", "--format=%s", "refs/ai/chat/dev"]);
    assert_eq!(log, "sync claude s-1\nattach claude s-1\n");
    let format = git(&r.path(), &["show", "refs/ai/chat/dev:format"]);
    assert_eq!(format, "2\n");
}

#[test]
fn an_unattached_session_is_not_attached() {
    let outside = TempDir::new().unwrap();
    let s = sync(outside.path(), "claude", "s-1", None).unwrap();
    assert_eq!(s, Synced::NotAttached, "outside a repository");
    let r = Repo::new();
    assert_eq!(
        sync(&r.path(), "claude", "s-1", None).unwrap(),
        Synced::NotAttached
    );
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let chat = r.chat();
    chat.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    assert_eq!(
        sync(&r.path(), "claude", "s-2", None).unwrap(),
        Synced::NotAttached
    );
    assert_eq!(chat.detach("claude", "s-1").unwrap(), Some("dev".into()));
    assert_eq!(chat.detach("claude", "s-1").unwrap(), None);
    let head = r.head("dev");
    assert_eq!(
        sync(&r.path(), "claude", "s-1", Some(&path)).unwrap(),
        Synced::NotAttached
    );
    assert_eq!(r.head("dev"), head);
    assert!(sync(&r.path(), "claude", "../s-1", None).is_err());
}

#[test]
fn a_session_without_a_transcript_yet_is_bound_and_synced_later() {
    let r = Repo::new();
    let path = r.dir.path().join("s-1.jsonl");
    let a = r
        .chat()
        .attach("dev", "claude", "s-1", Some(&path))
        .unwrap();
    assert!(a.created && a.added.is_empty());
    r.transcript("s-1", &lines("plain.jsonl"));
    assert_eq!(added(sync(&r.path(), "claude", "s-1", None).unwrap()), 0..4);
}

#[test]
fn a_second_session_grafts_onto_the_newest_item() {
    let r = Repo::new();
    let chat = r.chat();
    let p1 = r.transcript("s-1", &lines("plain.jsonl"));
    let p2 = r.transcript("s-2", &relabel(&lines("rewind.jsonl"), "b-"));
    chat.attach("dev", "claude", "s-1", Some(&p1)).unwrap();
    let a = chat.attach("dev", "claude", "s-2", Some(&p2)).unwrap();
    assert!(!a.created);
    assert_eq!(a.added.items, 4..14);
    let t = chat.thread("dev").unwrap();
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
    let list = chat.list().unwrap();
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
    let chat = r.chat();
    let all = lines("rewind.jsonl");
    let p1 = r.transcript("s-1", &all[..6]);
    chat.attach("dev", "claude", "s-1", Some(&p1)).unwrap();
    let n = chat
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
    let a = chat.attach("dev", "claude", "s-2", Some(&p2)).unwrap();
    let t = chat.thread("dev").unwrap();
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
    let chat = r.chat();
    let all = lines("interject.jsonl");
    let p = r.transcript("s-1", &all[..6]);
    let other = r.transcript("s-0", &relabel(&lines("plain.jsonl"), "z-"));
    chat.attach("dev", "claude", "s-0", Some(&other)).unwrap();
    chat.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    chat.detach("claude", "s-1").unwrap();
    let t = chat.thread("dev").unwrap();
    let s = t.snapshot().unwrap();
    let b = s.binding("claude", "s-1").unwrap().unwrap();
    assert!(!b.is_attached() && b.graft == Some(3));
    let before = s.len().unwrap();
    r.transcript("s-1", &all);
    let a = chat.attach("dev", "claude", "s-1", None).unwrap();
    assert!(!a.created);
    let t = chat.thread("dev").unwrap();
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
    let chat = r.chat();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    chat.attach("one", "claude", "s-1", Some(&p)).unwrap();
    let original = chat
        .thread("one")
        .unwrap()
        .snapshot()
        .unwrap()
        .items()
        .unwrap();
    assert_eq!(chat.detach("claude", "s-1").unwrap(), Some("one".into()));
    let a = chat.attach("two", "claude", "s-1", Some(&p)).unwrap();
    assert!(a.created);
    assert_eq!(
        a.added.items,
        0..4,
        "the new thread ingests the whole history"
    );
    assert_eq!(
        chat.attached("claude", "s-1").unwrap().unwrap().0.name(),
        "two"
    );
    let t = chat.thread("one").unwrap();
    let one = t.snapshot().unwrap();
    assert!(!one.binding("claude", "s-1").unwrap().unwrap().is_attached());
    assert_eq!(one.items().unwrap(), original);
    assert_eq!(
        chat.thread("two")
            .unwrap()
            .snapshot()
            .unwrap()
            .items()
            .unwrap(),
        original
    );
    let head = r.head("two");
    assert!(
        chat.attach("two", "claude", "s-1", None)
            .unwrap()
            .added
            .is_empty()
    );
    assert_eq!(
        r.head("two"),
        head,
        "same-thread reattachment with no new data is a no-op"
    );
    let names: Vec<_> = chat
        .threads()
        .unwrap()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(names, ["one", "two"]);
    match sync(&r.path(), "claude", "s-1", None).unwrap() {
        Synced::Attached { thread, added } => assert!(thread == "two" && added.is_empty()),
        Synced::NotAttached => panic!("attached to two"),
    }
}

#[test]
fn attaching_elsewhere_is_rejected_before_reading_new_sources() {
    let r = Repo::new();
    let chat = r.chat();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    chat.attach("one", "claude", "s-1", Some(&path)).unwrap();
    let before = r.head("one");
    let agents = path.with_extension("").join("subagents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(agents.join("agent-.invalid.jsonl"), "").unwrap();

    let error = chat.attach("two", "claude", "s-1", None).unwrap_err();
    assert!(error.to_string().contains("Run ai chat detach"));

    assert_eq!(r.head("one"), before);
    assert!(!chat.thread("two").unwrap().exists().unwrap());
    let attached = chat.attached("claude", "s-1").unwrap().unwrap();
    assert_eq!(attached.0.name(), "one");
    assert_eq!(attached.1.transcript.as_deref(), Some(path.as_path()));
}

#[test]
fn attaching_elsewhere_is_rejected_without_changing_either_thread() {
    let r = Repo::new();
    let chat = r.chat();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    chat.attach("one", "claude", "s-1", Some(&path)).unwrap();
    let before = r.head("one");
    chat.attach("two", "claude", "s-2", None).unwrap();
    let destination = r.head("two");

    let error = chat.attach("two", "claude", "s-1", None).unwrap_err();
    assert!(error.to_string().contains("Run ai chat detach"));

    assert_eq!(r.head("one"), before);
    assert_eq!(r.head("two"), destination);
    assert_eq!(
        chat.attached("claude", "s-1").unwrap().unwrap().0.name(),
        "one"
    );
    assert!(matches!(
        chat.sync("claude", "s-1", None).unwrap(),
        Synced::Attached { thread, .. } if thread == "one"
    ));
}

#[test]
fn attaching_archives_complete_original_session_files() {
    let r = Repo::new();
    let chat = r.chat();
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

    chat.attach("dev", "claude", "s-1", Some(&path)).unwrap();

    let thread = chat.thread("dev").unwrap();
    let snapshot = thread.snapshot().unwrap();
    for (name, bytes) in &files {
        let stored = format!("sources/claude/s-1/{name}");
        assert_blob(&snapshot, &stored, bytes);
    }
    assert!(
        snapshot
            .as_ref()
            .entry("sources/claude/s-1/unrelated.json")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        snapshot.items().unwrap(),
        claude::read(&path, "s-1").unwrap()
    );
}

#[test]
fn sync_archives_source_changes_without_new_projected_items() {
    let r = Repo::new();
    let chat = r.chat();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    chat.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let before = r.head("dev");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"future-event\",\"payload\":true}\nunfinished \xff")
        .unwrap();
    let original = fs::read(&path).unwrap();

    assert!(matches!(
        chat.sync("claude", "s-1", None).unwrap(),
        Synced::Attached { added, .. } if added.is_empty()
    ));

    let thread = chat.thread("dev").unwrap();
    let snapshot = thread.snapshot().unwrap();
    assert_blob(&snapshot, "sources/claude/s-1/s-1.jsonl", &original);
    assert_ne!(r.head("dev"), before);
    let archived = r.head("dev");
    chat.sync("claude", "s-1", None).unwrap();
    assert_eq!(r.head("dev"), archived, "unchanged sources add no commit");
}

#[test]
fn original_files_survive_source_changes_and_disappearance() {
    let r = Repo::new();
    let chat = r.chat();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let original = fs::read(&path).unwrap();
    let artifact = path.with_extension("").join("tool-results/output.bin");
    fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    fs::write(&artifact, b"original\0artifact\xff").unwrap();
    chat.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let thread = chat.thread("dev").unwrap();
    let first = thread.snapshot().unwrap().commit().unwrap();
    let mut changed = original.clone();
    changed.extend_from_slice(b"{\"type\":\"future-event\"}\n");
    fs::write(&path, &changed).unwrap();
    fs::write(&artifact, b"changed artifact").unwrap();
    chat.sync("claude", "s-1", None).unwrap();
    fs::remove_file(&path).unwrap();
    fs::remove_file(&artifact).unwrap();

    chat.sync("claude", "s-1", None).unwrap();

    let latest = thread.snapshot().unwrap();
    assert_blob(&latest, "sources/claude/s-1/s-1.jsonl", &changed);
    assert_blob(
        &latest,
        "sources/claude/s-1/s-1/tool-results/output.bin",
        b"changed artifact",
    );
    let original_snapshot = thread.snapshot_at(first).unwrap();
    assert_blob(
        &original_snapshot,
        "sources/claude/s-1/s-1.jsonl",
        &original,
    );
    assert_blob(
        &original_snapshot,
        "sources/claude/s-1/s-1/tool-results/output.bin",
        b"original\0artifact\xff",
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
    let chat = r.chat();
    let a = chat.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let read = read("claude", "s-1", Some(&p)).unwrap();
    assert_eq!(a.added.items, 0..read.items.len() as u64);
    assert_eq!(a.added.agent_items, 6);
    let thread = chat.thread("dev").unwrap();
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
    let head = r.head("dev");
    let again = sync(&r.path(), "claude", "s-1", None).unwrap();
    assert!(matches!(again, Synced::Attached { added, .. } if added.is_empty()));
    assert_eq!(r.head("dev"), head);
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
    let chat = r.chat();
    chat.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let thread = chat.thread("dev").unwrap();
    let before = thread.snapshot().unwrap().agents().unwrap();
    assert!(before[0].call.is_some());
    fs::remove_file(&path).unwrap();

    chat.sync("claude", "s-1", None).unwrap();

    assert_eq!(thread.snapshot().unwrap().agents().unwrap(), before);
}

#[test]
fn lines_are_built_once() {
    let r = Repo::new();
    let chat = r.chat();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    chat.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let t = chat.thread("dev").unwrap();
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
    let chat = r.chat();
    let (a, b) = (chat.thread("a").unwrap(), chat.thread("b").unwrap());
    let held = a.try_lock().unwrap();
    assert!(held.is_some());
    assert!(r.chat().thread("a").unwrap().try_lock().unwrap().is_none());
    assert!(b.try_lock().unwrap().is_some());
    drop(held);
    assert!(a.try_lock().unwrap().is_some());
    assert!(r.path().join(".git/ai/chat/a.lock").is_file());
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
    let chat = Chat::find(&sub).unwrap().unwrap();
    chat.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let refs = git(
        &r.path(),
        &["for-each-ref", "--format=%(refname)", "refs/ai/chat"],
    );
    assert_eq!(refs, "refs/ai/chat/dev\n");
    assert!(chat.thread("dev").unwrap().try_lock().unwrap().is_some());
    assert!(r.path().join(".git/ai/chat/dev.lock").is_file());
    let main = r.chat();
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
fn a_newer_thread_is_refused() {
    let r = Repo::new();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    r.chat().attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let path = r.path();
    let old = git(&path, &["rev-parse", "refs/ai/chat/dev:format"]);
    let new = git_in(&path, &["hash-object", "-w", "--stdin"], "3\n");
    let tree = git(&path, &["ls-tree", "refs/ai/chat/dev"]).replace(old.trim(), new.trim());
    let tree = git_in(&path, &["mktree"], &tree);
    let commit = git(
        &path,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            "refs/ai/chat/dev",
            "-m",
            "future",
        ],
    );
    git(&path, &["update-ref", "refs/ai/chat/dev", commit.trim()]);
    let chat = r.chat();
    let want = "This thread is format 3; this ai reads up to 2. Upgrade ai.";
    let snap = chat.thread("dev").unwrap().snapshot().map(|_| ());
    assert_eq!(snap.unwrap_err().to_string(), want);
    let s = sync(&path, "claude", "s-1", None).map(|_| ());
    assert_eq!(s.unwrap_err().to_string(), want);
    assert_eq!(r.head("dev").unwrap(), commit.trim());
    assert!(chat.thread("dev").unwrap().upgrade().is_err());
    assert_eq!(r.head("dev").unwrap(), commit.trim());
}

#[test]
fn structured_indexes_keep_source_ids_exact() {
    let r = Repo::new();
    let path = r.transcript("s-1", &relabel(&lines("plain.jsonl"), "with space "));
    let chat = r.chat();
    chat.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let thread = chat.thread("dev").unwrap();
    let snapshot = thread.snapshot().unwrap();

    assert_eq!(
        snapshot.items().unwrap(),
        claude::read(&path, "s-1").unwrap()
    );
    assert!(
        snapshot
            .heads()
            .unwrap()
            .iter()
            .all(|head| head.entry.starts_with("with space "))
    );
    let index: serde_json::Value =
        serde_json::from_slice(&snapshot.as_ref().read("index/0000/00").unwrap()).unwrap();
    assert_eq!(index[0]["entry"], "with space u1");
    let item: serde_json::Value =
        serde_json::from_slice(&snapshot.as_ref().read("items/0000/00").unwrap()).unwrap();
    assert_eq!(item["kind"], "user");
}

#[test]
fn structured_indexes_reject_invalid_positions_and_parents() {
    use ai_tools::git::{Changes, Ref};

    let r = Repo::new();
    let path = r.transcript("s-1", &lines("plain.jsonl"));
    let chat = r.chat();
    chat.attach("dev", "claude", "s-1", Some(&path)).unwrap();
    let thread = chat.thread("dev").unwrap();
    let snapshot = thread.snapshot().unwrap();
    let original: serde_json::Value =
        serde_json::from_slice(&snapshot.as_ref().read("index/0000/00").unwrap()).unwrap();
    let mut repo = gix::open(r.path()).unwrap();
    repo.committer_or_set_fallback("test", "test@localhost")
        .unwrap();
    let reference = Ref::new(repo, "refs/ai/chat/dev".into(), "thread", 2);
    for (field, value) in [("seq", 99), ("parent", 0)] {
        let mut broken = original.clone();
        broken[0][field] = value.into();
        reference
            .mutate(
                "invalid index fixture",
                |commit| reference.raw(commit),
                |_| {
                    let mut changes = Changes::default();
                    changes.put("index/0000/00".into(), serde_json::to_vec(&broken).unwrap());
                    Ok(((), changes))
                },
            )
            .unwrap();

        let snapshot = thread.snapshot().unwrap();
        assert!(snapshot.heads().is_err(), "invalid {field}");
        assert!(snapshot.item(0).is_err(), "invalid {field}");
        assert!(snapshot.tree().is_err(), "invalid {field}");
    }
    reference
        .mutate(
            "empty index fixture",
            |commit| reference.raw(commit),
            |_| {
                let mut changes = Changes::default();
                changes.put("index/0000/00".into(), b"[]".to_vec());
                Ok(((), changes))
            },
        )
        .unwrap();
    let snapshot = thread.snapshot().unwrap();
    assert!(snapshot.heads().is_err());
    assert!(snapshot.items().is_err());
}

#[test]
fn a_v1_thread_exports_to_json_without_rewriting_its_history() {
    use ai_tools::git::{Changes, Ref};
    use clap::Parser;

    let r = Repo::new();
    let mut repo = gix::open(r.path()).unwrap();
    repo.committer_or_set_fallback("test", "test@localhost")
        .unwrap();
    let old = Ref::new(repo, "refs/ai/chat/dev".into(), "thread", 1);
    let raw = lines("plain.jsonl");
    let (_, source) = old.mutate("legacy thread", |commit| old.raw(commit), |_| {
        let mut changes = Changes::default();
        for (path, bytes) in [
            ("index/0000/00", b"ts harness session entry parent\n2026-10-14T10:00:00Z claude s-1 u1 \n2026-10-14T10:00:03Z claude s-1 a2 0\n".to_vec()),
            ("items/0000/00", b"kind user\n\nWhat does the config loader do?".to_vec()),
            ("items/0000/01", b"kind text\n\nIt reads `config.toml`, then applies `APP_*` environment overrides.".to_vec()),
            ("raw/0000/00", raw[2].trim_end_matches('\n').as_bytes().to_vec()),
            ("raw/0000/01", raw[6].trim_end_matches('\n').as_bytes().to_vec()),
            ("sessions/claude/s-1", b"transcript /missing/s-1.jsonl\ntip 1\nattached 2026-10-14T10:00:00Z\n\n".to_vec()),
            ("agents/claude/agent-1/agent", b"type general-purpose\ndescription Survey storage\ncall 1\n\n".to_vec()),
            ("agents/claude/agent-1/index/0000/00", b"ts harness session entry parent\n2026-10-14T10:00:02Z claude s-1 sub-1 \n".to_vec()),
            ("agents/claude/agent-1/items/0000/00", b"kind text\n\nagent answer".to_vec()),
            ("agents/claude/agent-1/raw/0000/00", b"{\"original\":\"agent\"}".to_vec()),
            ("lines/1/0000/01", b"ts 2026-10-14T10:00:03Z\nkind exchange\nagent claude-code\nmodel sonnet\nsession summary-1\n\nuser: config loader; ai: explains config and overrides".to_vec()),
            ("sources/claude/s-1/s-1/artifact.bin", vec![0, 255, 128, 10]),
        ] {
            changes.put(path.into(), bytes);
        }
        Ok(((), changes))
    }).unwrap();
    let first = source.commit().unwrap();
    let raw_id = source.entry("raw/0000/00").unwrap();
    let artifact_id = source.entry("sources/claude/s-1/s-1/artifact.bin").unwrap();
    let chat = r.chat();
    let thread = chat.thread("dev").unwrap();
    assert!(
        thread
            .snapshot()
            .err()
            .unwrap()
            .to_string()
            .contains("ai chat upgrade dev")
    );

    let mut output = Vec::new();
    let result = ai_tools::chat::Cli::parse_from(["chat", "upgrade", "dev"])
        .run(&r.path(), &mut output)
        .unwrap();
    assert_eq!(result, std::process::ExitCode::SUCCESS);
    assert_eq!(output, b"Upgraded dev to format 2.\n");

    let snapshot = thread.snapshot().unwrap();
    assert_eq!(snapshot.len().unwrap(), 2);
    assert_eq!(entries(&snapshot.items().unwrap()), ["u1", "a2"]);
    assert_eq!(
        snapshot.binding("claude", "s-1").unwrap().unwrap().tip,
        Some(1)
    );
    assert_eq!(snapshot.agents().unwrap()[0].description, "Survey storage");
    assert_eq!(
        snapshot.agent_items("claude", "agent-1").unwrap()[0].kind,
        Kind::Text("agent answer".into())
    );
    assert_eq!(
        snapshot
            .line(Key { level: 1, last: 1 })
            .unwrap()
            .unwrap()
            .who
            .model,
        "sonnet"
    );
    assert_eq!(snapshot.as_ref().entry("raw/0000/00").unwrap(), raw_id);
    assert_eq!(
        snapshot
            .as_ref()
            .entry("sources/claude/s-1/s-1/artifact.bin")
            .unwrap(),
        artifact_id
    );
    assert_eq!(
        git(&r.path(), &["rev-parse", "refs/ai/chat/dev^"]).trim(),
        first.to_string()
    );
    assert_eq!(
        old.raw(Some(first))
            .unwrap()
            .read("format")
            .unwrap()
            .as_ref(),
        b"1\n"
    );
    let converted = r.head("dev");
    assert!(!thread.upgrade().unwrap());
    assert_eq!(r.head("dev"), converted);
}

#[test]
fn a_corrupt_v1_sequence_is_not_published_as_v2() {
    use ai_tools::git::{Changes, Ref};

    for missing in ["items/0000/00", "raw/0000/00"] {
        let r = Repo::new();
        let mut repo = gix::open(r.path()).unwrap();
        repo.committer_or_set_fallback("test", "test@localhost")
            .unwrap();
        let old = Ref::new(repo, "refs/ai/chat/dev".into(), "thread", 1);
        old.mutate(
            "corrupt v1 fixture",
            |commit| old.raw(commit),
            |_| {
                let mut changes = Changes::default();
                for (path, bytes) in [
                    (
                        "index/0000/00",
                        b"ts harness session entry parent\n2026-10-14T10:00:00Z claude s-1 u1 \n"
                            .as_slice(),
                    ),
                    ("items/0000/00", b"kind user\n\nprompt".as_slice()),
                    ("raw/0000/00", b"original source".as_slice()),
                ] {
                    if path != missing {
                        changes.put(path.into(), bytes.to_vec());
                    }
                }
                Ok(((), changes))
            },
        )
        .unwrap();
        let original = r.head("dev");

        assert!(
            r.chat().thread("dev").unwrap().upgrade().is_err(),
            "missing {missing}"
        );

        assert_eq!(r.head("dev"), original);
        assert_eq!(
            old.raw(old.head().unwrap())
                .unwrap()
                .read("format")
                .unwrap()
                .as_ref(),
            b"1\n"
        );
    }
}

#[test]
fn names_and_ids_are_checked() {
    let r = Repo::new();
    let chat = r.chat();
    for bad in ["", "-x", "a/b", "a.b", "a b"] {
        assert!(chat.thread(bad).is_err(), "{bad}");
    }
    assert!(chat.attach("dev", "claude", "a/b", None).is_err());
    assert!(chat.attach("dev", "", "s-1", None).is_err());
    assert!(!chat.thread("dev").unwrap().exists().unwrap());
}

const CHILD: &str = "AI_CHAT_TEST_SYNC";

/// Run by [`two_processes_syncing_one_thread_lose_nothing`] as a child process: syncs the
/// session `$AI_CHAT_TEST_SYNC` names (`<repo> <session>`); nothing otherwise.
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
    r.chat().attach("dev", "claude", "s-a", Some(&pa)).unwrap();
    r.chat().attach("dev", "claude", "s-b", Some(&pb)).unwrap();
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
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for mut k in kids {
            assert!(k.wait().unwrap().success());
        }
    }
    let t = r.chat().thread("dev").unwrap();
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
    let commits = git(&r.path(), &["rev-list", "--count", "refs/ai/chat/dev"]);
    let syncs = git(&r.path(), &["log", "--format=%s", "refs/ai/chat/dev"]);
    let syncs = syncs.lines().filter(|l| l.starts_with("sync")).count();
    assert_eq!(commits.trim().parse::<usize>().unwrap(), syncs + 2);
    assert!(
        syncs <= 2 * steps,
        "a losing sync with nothing new commits nothing"
    );
}
