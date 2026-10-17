include!("chat.rs");

#[test]
fn core_probe_failed_move_loses_attachment() {
    let r = Repo::new();
    let chat = r.chat();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    chat.attach("one", "claude", "s-1", Some(&p)).unwrap();
    let subs = r.dir.path().join("s-1/subagents");
    fs::create_dir_all(&subs).unwrap();
    fs::write(subs.join("agent-.bad.jsonl"), "").unwrap();
    let err = chat.attach("two", "claude", "s-1", Some(&p)).unwrap_err();
    eprintln!("move failed: {err:#}");
    assert!(chat.attached("claude", "s-1").unwrap().is_none(), "failure detached original binding");
    assert!(!chat.thread("two").unwrap().exists().unwrap());
}

#[test]
fn core_probe_concurrent_attach_creates_two_active_bindings() {
    let r = Repo::new();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
    let handles: Vec<_> = (0..12).map(|n| {
        let barrier = barrier.clone();
        let path = r.path();
        std::thread::spawn(move || {
            let chat = Chat::find(&path).unwrap().unwrap();
            barrier.wait();
            chat.attach(&format!("thread{n}"), "claude", "s-1", None).unwrap();
        })
    }).collect();
    for h in handles { h.join().unwrap(); }
    let chat = r.chat();
    let count = || chat.threads().unwrap().into_iter().filter(|t| t.snapshot().unwrap().binding("claude", "s-1").unwrap().is_some_and(|b| b.is_attached())).count();
    let active = count();
    eprintln!("simultaneous active bindings: {active}");
    assert!(active > 1);
    chat.detach("claude", "s-1").unwrap();
    assert!(chat.attached("claude", "s-1").unwrap().is_some(), "detach reveals another attached thread");
}

#[test]
fn core_probe_stale_sync_regresses_tip() {
    let r = Repo::new();
    let chat = r.chat();
    let all = lines("plain.jsonl");
    let old = r.transcript("old", &all[..3]);
    chat.attach("dev", "claude", "s-1", Some(&old)).unwrap();
    let mut old_data = all[..3].concat();
    let dropped = "{\"uuid\":\"noise\",\"type\":\"progress\"}\n";
    old_data.push_str(&dropped.repeat(2_000_000));
    fs::write(&old, old_data).unwrap();
    let path = r.path();
    let old_copy = old.clone();
    let h = std::thread::spawn(move || Chat::find(&path).unwrap().unwrap().sync("claude", "s-1", Some(&old_copy)).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(100));
    fs::write(&old, all.concat()).unwrap();
    chat.sync("claude", "s-1", Some(&old)).unwrap();
    h.join().unwrap();
    let thread = chat.thread("dev").unwrap();
    let snapshot = thread.snapshot().unwrap();
    let b = snapshot.binding("claude", "s-1").unwrap().unwrap();
    eprintln!("stored items: {}, binding tip: {:?}, old parsed items: {}", snapshot.len().unwrap(), b.tip, claude::read(&old, "s-1").unwrap().len());
    assert_eq!(snapshot.len().unwrap(), 4);
    assert_eq!(b.tip, Some(0), "the older read overwrote the newer tip");
    assert_eq!(b.transcript.as_deref(), Some(old.as_path()));
}

#[test]
fn core_probe_ref_lock_failed_move_loses_attachment() {
    let r = Repo::new();
    let chat = r.chat();
    let p = r.transcript("s-1", &lines("plain.jsonl"));
    chat.attach("one", "claude", "s-1", Some(&p)).unwrap();
    let lock_dir = r.path().join(".git/refs/ai/chat");
    fs::create_dir_all(&lock_dir).unwrap();
    fs::write(lock_dir.join("two.lock"), "held").unwrap();
    let err = chat.attach("two", "claude", "s-1", Some(&p)).unwrap_err();
    eprintln!("move failed: {err:#}");
    assert!(chat.attached("claude", "s-1").unwrap().is_none());
}

#[test]
fn core_probe_stale_child() {
    let Ok(path) = std::env::var("CORE_PROBE_REPO") else { return; };
    Chat::find(Path::new(&path)).unwrap().unwrap().sync("claude", "s-1", None).unwrap();
}

#[test]
fn core_probe_append_only_stale_sync_regresses_tip() {
    let r = Repo::new();
    let chat = r.chat();
    let all = lines("plain.jsonl");
    let p = r.transcript("s-1", &all[..3]);
    chat.attach("dev", "claude", "s-1", Some(&p)).unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(&p).unwrap();
    let dropped = "{\"uuid\":\"noise\",\"type\":\"progress\"}\n";
    file.write_all(dropped.repeat(2_000_000).as_bytes()).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "core_probe_stale_child", "--test-threads=1"])
        .env("CORE_PROBE_REPO", r.path())
        .stdout(Stdio::null()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGSTOP) }, 0);
    file.write_all(all[3..].concat().as_bytes()).unwrap();
    chat.sync("claude", "s-1", None).unwrap();
    let thread = chat.thread("dev").unwrap();
    assert_eq!(thread.snapshot().unwrap().tip("claude", "s-1").unwrap(), Some(3));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGCONT) }, 0);
    assert!(child.wait().unwrap().success());
    let s = thread.snapshot().unwrap();
    eprintln!("append-only same-path sync: len={}, tip={:?}", s.len().unwrap(), s.tip("claude", "s-1").unwrap());
    assert_eq!(s.len().unwrap(), 4);
    assert_eq!(s.tip("claude", "s-1").unwrap(), Some(0));
}
