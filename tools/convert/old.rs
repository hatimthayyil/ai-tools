//! Reading a project's agents as ai 0.17 kept them, agent store format 4, and writing them as a
//! store of the format this ai reads. Each file format 4 kept as a directory of segments becomes
//! one file of the same bytes. A thread's copies of its sessions' files become records of the
//! harness's own files; a copy the harness no longer holds whole is restored into the harness's
//! files, where its record points.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, FileType},
    io::{self, Write},
    path::{Path, PathBuf},
};

use ai_tools::{
    agent::{
        AGENTS, ATTACHMENTS, FORMAT, ITEMS, LINES, SESSIONS, SOURCES, THREADS, check_name,
        record::{Event, Pointer, State, Track},
        source,
    },
    backup,
    file::{self, AtPath, pretty, sha256},
    jsonl,
};
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use serde::Deserialize;

/// The agent store format ai 0.17 wrote, the one this converts.
pub const FROM: u32 = 4;

/// A change of a session's attachment as format 4 kept it, for every thread in one file.
#[derive(Deserialize)]
struct OldEvent {
    harness: String,
    session: String,
    at: Timestamp,
    state: State,
    thread: Option<String>,
}

/// A session's branch in one thread as format 4 kept it.
#[derive(Deserialize)]
struct OldTrack {
    harness: String,
    session: String,
    transcript: Option<PathBuf>,
    graft: Option<u64>,
    tip: Option<u64>,
}

/// What [`build`] made of one thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub name: String,
    /// Its items: complete lines of its items file.
    pub items: u64,
}

/// A session's file the harness no longer held whole, written back from its copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restore {
    /// The copy, in the old store.
    pub copy: PathBuf,
    /// Where its bytes go.
    pub to: PathBuf,
}

/// What [`build`] made, or with `restore` off would make.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Built {
    pub threads: Vec<Thread>,
    /// The copies whose files the harness still holds, now pointed at.
    pub pointed: usize,
    /// The copies the harness no longer held whole.
    pub restored: Vec<Restore>,
}

/// The format of the store `agents`: an error unless it is [`FROM`].
pub fn check(agents: &Path) -> Result<()> {
    match file::format(agents, "agent")? {
        Some(FROM) => Ok(()),
        v => bail!(
            "Cannot convert {}: its agents are format {}; this converts format {FROM}, as ai 0.17 \
             wrote it.",
            pretty(agents),
            v.map_or("unknown".into(), |v| v.to_string())
        ),
    }
}

/// The writers' and compactors' locks of every thread in the store `agents`, so no ai 0.17
/// writes while this reads; an error naming a thread one of them still holds. ai 0.17 makes a
/// lock's file before taking it, so a lock without its file is held by none.
pub fn lock(agents: &Path) -> Result<Vec<file::Lock>> {
    let mut files = vec![agents.join("sessions.lock")];
    for n in names(&agents.join(THREADS))? {
        files.push(agents.join(format!("{n}.write.lock")));
        files.push(agents.join(format!("{n}.lock")));
    }
    let mut locks = Vec::new();
    for f in files.into_iter().filter(|f| f.is_file()) {
        match file::try_lock(&f)? {
            Some(l) => locks.push(l),
            None => bail!(
                "An ai is writing this project's agents ({} is held). Let it finish, then try \
                 again.",
                pretty(&f)
            ),
        }
    }
    Ok(locks)
}

/// The thread names among the directories of `dir`; none if it does not exist.
fn names(dir: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for (name, _, kind) in entries_if(dir)? {
        if kind.is_dir() && check_name(&name).is_ok() {
            out.insert(name);
        }
    }
    Ok(out)
}

/// Builds at `to` the store of format [`FORMAT`] from the format-4 store `agents`: each thread's
/// own files joined, its attachment changes taken from the store-wide file, its session tracks
/// and sources pointed at the harnesses' files. With `restore`, a copy the harness no longer
/// holds whole is written back first; without it, nothing outside `to` changes and the record
/// points where it would go.
pub fn build(agents: &Path, to: &Path, restore: bool) -> Result<Built> {
    file::ensure_format(to, FORMAT)?;
    let mut events: BTreeMap<String, Vec<Event>> = BTreeMap::new();
    let mut owner: BTreeMap<(String, String), String> = BTreeMap::new();
    let file = agents.join(SESSIONS);
    let all = complete(joined(&file)?);
    for (i, l) in jsonl::lines(&all).enumerate() {
        let e: OldEvent = jsonl::decode(l)
            .with_context(|| format!("{}: record {} is corrupt", pretty(&file), i + 1))?;
        let key = (e.harness.clone(), e.session.clone());
        let thread = match (e.state, e.thread) {
            (State::Attached, Some(t)) => {
                if let Some(was) = owner.insert(key, t.clone()).filter(|was| *was != t) {
                    events.entry(was).or_default().push(Event {
                        harness: e.harness.clone(),
                        session: e.session.clone(),
                        at: e.at,
                        state: State::Detached,
                    });
                }
                t
            }
            (State::Attached, None) => bail!(
                "{}: record {} attaches a session to no thread.",
                pretty(&file),
                i + 1
            ),
            (State::Detached, _) => match owner.remove(&key) {
                Some(t) => t,
                None => continue,
            },
        };
        events.entry(thread).or_default().push(Event {
            harness: e.harness,
            session: e.session,
            at: e.at,
            state: e.state,
        });
    }
    let mut built = Built::default();
    let threads = agents.join(THREADS);
    for name in names(&threads)? {
        let dst = to.join(THREADS).join(&name);
        let items = thread(&threads.join(&name), &dst, restore, &mut built)?;
        jsonl::append(
            &dst.join(ATTACHMENTS),
            events.get(&name).map_or(&[][..], Vec::as_slice),
        )?;
        built.threads.push(Thread { name, items });
    }
    Ok(built)
}

/// One format-4 thread at `src`, built at `dst`; how many items it holds.
fn thread(src: &Path, dst: &Path, restore: bool, built: &mut Built) -> Result<u64> {
    fs::create_dir_all(dst).at(dst)?;
    let mut items = 0;
    let mut tracks = Vec::new();
    for (name, from, _) in entries(src)? {
        match name.as_str() {
            ITEMS => items = jsonl::count(&join(&from, &dst.join(&name))?),
            LINES | AGENTS => {
                join(&from, &dst.join(&name))?;
            }
            "agents" => join_subagents(&from, &dst.join(&name))?,
            "calls" => copy_tree(&from, &dst.join(&name))?,
            SESSIONS => {
                let b = complete(joined(&from)?);
                for (i, l) in jsonl::lines(&b).enumerate() {
                    let t: OldTrack = jsonl::decode(l).with_context(|| {
                        format!("{}: record {} is corrupt", pretty(&from), i + 1)
                    })?;
                    tracks.push(t);
                }
            }
            "sources" => {}
            _ => bail!(
                "Cannot convert {}: it is not part of a format-{FROM} thread.",
                pretty(&from)
            ),
        }
    }
    let mut pointers = Vec::new();
    let host = backup::host()?;
    let read = Timestamp::now();
    for (harness, by_harness, _) in entries_if(&src.join("sources"))? {
        for (session, dir, _) in entries(&by_harness)? {
            let transcript = tracks
                .iter()
                .rfind(|t| t.harness == harness && t.session == session)
                .and_then(|t| t.transcript.clone())
                .with_context(|| {
                    format!(
                        "Cannot convert {}: no session record names its transcript.",
                        pretty(&dir)
                    )
                })?;
            let base = transcript
                .parent()
                .context("A transcript needs a parent directory.")?;
            let companion = transcript.with_extension("");
            for (rel, copy) in copies(&dir)? {
                let bytes = joined(&copy)?;
                let raw = base.join(&rel);
                let at = match holds(&raw, &bytes)? {
                    true => {
                        built.pointed += 1;
                        raw
                    }
                    false => {
                        let to = place(&raw, &companion, &rel, &bytes)?;
                        if restore {
                            write_back(&to, &bytes)?;
                        }
                        built.restored.push(Restore {
                            copy: copy.clone(),
                            to: to.clone(),
                        });
                        to
                    }
                };
                pointers.push(Pointer {
                    harness: harness.clone(),
                    session: session.clone(),
                    host: host.clone(),
                    path: source::real(&at),
                    from: 0,
                    len: bytes.len() as u64,
                    sha256: sha256(&bytes),
                    at: read,
                });
            }
        }
    }
    let tracks: Vec<Track> = tracks
        .into_iter()
        .map(|t| Track {
            config: t
                .transcript
                .as_deref()
                .and_then(|p| source::config(&t.harness, p)),
            transcript: t.transcript.map(|p| source::real(&p)),
            harness: t.harness,
            session: t.session,
            graft: t.graft,
            tip: t.tip,
        })
        .collect();
    jsonl::append(&dst.join(SESSIONS), &tracks)?;
    jsonl::append(&dst.join(SOURCES), &pointers)?;
    Ok(items)
}

/// Every copied file under a session's sources `dir`, by its path there: each a segmented file.
fn copies(dir: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
        for (_, p, kind) in entries(dir)? {
            if is_segmented(&p) {
                let rel = p.strip_prefix(root).expect("under the walk's root");
                out.push((rel.to_path_buf(), p));
            } else if kind.is_dir() {
                walk(root, &p, out)?;
            } else {
                bail!(
                    "Cannot convert {}: format {FROM} keeps each copied file as a directory of \
                     segments.",
                    pretty(&p)
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

/// Whether `p` is a file as format 4 keeps one: a directory holding segment files.
fn is_segmented(p: &Path) -> bool {
    fs::read_dir(p).is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| e.path().is_file())))
}

/// Every byte of the segmented file `p`, its segments joined in order; empty if `p` does not
/// exist. Segments are numbered from `000000`, each but the last ending a line.
pub fn joined(p: &Path) -> Result<Vec<u8>> {
    if !p.exists() {
        return Ok(Vec::new());
    }
    if !p.is_dir() {
        bail!(
            "Cannot convert {}: format {FROM} keeps it as a directory of segments.",
            pretty(p)
        );
    }
    let segments = entries(p)?;
    let mut out = Vec::new();
    for (i, (name, seg, kind)) in segments.iter().enumerate() {
        let want = match p.extension() {
            Some(ext) => format!("{i:06}.{}", ext.to_string_lossy()),
            None => format!("{i:06}"),
        };
        if *name != want || !kind.is_file() {
            bail!(
                "Cannot convert {}: its segments are not numbered from 0.",
                pretty(p)
            );
        }
        let b = fs::read(seg).at(seg)?;
        if i + 1 < segments.len() && b.last() != Some(&b'\n') {
            bail!(
                "Cannot convert {}: segment {i} ends inside a line.",
                pretty(p)
            );
        }
        out.extend(b);
    }
    Ok(out)
}

/// `b`'s complete lines, as [`jsonl::read`] gives a file's.
fn complete(mut b: Vec<u8>) -> Vec<u8> {
    b.truncate(jsonl::complete(&b));
    b
}

/// Writes the segmented file `from` at `to` as one file of the same bytes, and returns them.
fn join(from: &Path, to: &Path) -> Result<Vec<u8>> {
    let b = joined(from)?;
    fs::write(to, &b).at(to)?;
    Ok(b)
}

/// Writes the subagents' files under `from`, `<harness>/<agent>.jsonl` each segmented, under
/// `to` as one file each.
fn join_subagents(from: &Path, to: &Path) -> Result<()> {
    for (harness, dir, kind) in entries(from)? {
        if !kind.is_dir() {
            bail!(
                "Cannot convert {}: it is not part of a format-{FROM} thread.",
                pretty(&dir)
            );
        }
        let out = to.join(&harness);
        fs::create_dir_all(&out).at(&out)?;
        for (name, file, _) in entries(&dir)? {
            join(&file, &out.join(name))?;
        }
    }
    Ok(())
}

/// Whether the file at `raw` begins with `bytes`.
fn holds(raw: &Path, bytes: &[u8]) -> Result<bool> {
    match fs::read(raw) {
        Ok(have) => Ok(have.starts_with(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).at(raw),
    }
}

/// Where the copy `bytes` of the harness file `raw`, at `rel` in its transcript's directory,
/// goes back: `raw` when it is gone, else `restored/<rel>` in the session's `companion`
/// directory, which every sync reads, numbered `restored-2`, … when that path holds other bytes.
/// A path that already holds `bytes` is where they are.
fn place(raw: &Path, companion: &Path, rel: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let mut at = raw.to_path_buf();
    let mut n = 0;
    while at.exists() {
        if fs::read(&at).at(&at)? == bytes {
            return Ok(at);
        }
        n += 1;
        at = companion
            .join(match n {
                1 => "restored".to_string(),
                n => format!("restored-{n}"),
            })
            .join(rel);
    }
    Ok(at)
}

/// Writes `bytes` at `at`, a path [`place`] chose, unless it holds them already, through to the
/// disk, and checks it reads back the same.
fn write_back(at: &Path, bytes: &[u8]) -> Result<()> {
    if at.exists() {
        return Ok(());
    }
    if let Some(dir) = at.parent() {
        fs::create_dir_all(dir).at(dir)?;
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(at)
        .at(at)?;
    f.write_all(bytes).at(at)?;
    f.sync_all().at(at)?;
    if fs::read(at).at(at)? != bytes {
        bail!("Restored {} reads back different bytes.", pretty(at));
    }
    Ok(())
}

/// The entries of `dir`: name, path, type, by name.
fn entries(dir: &Path) -> Result<Vec<(String, PathBuf, FileType)>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).at(dir)? {
        let e = e.at(dir)?;
        let name = e
            .file_name()
            .into_string()
            .map_err(|n| anyhow!("{}: a name is not UTF-8: {n:?}", pretty(dir)))?;
        out.push((name, e.path(), e.file_type().at(dir)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// [`entries`], none if `dir` does not exist.
fn entries_if(dir: &Path) -> Result<Vec<(String, PathBuf, FileType)>> {
    match dir.exists() {
        true => entries(dir),
        false => Ok(Vec::new()),
    }
}

/// Copies `from` to `to`: a directory with everything in it, links as links.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from).at(from)?;
    if meta.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from).at(from)?, to).at(to)?;
    } else if meta.is_dir() {
        fs::create_dir_all(to).at(to)?;
        for (name, p, _) in entries(from)? {
            copy_tree(&p, &to.join(name))?;
        }
    } else {
        fs::copy(from, to).at(from)?;
    }
    Ok(())
}
